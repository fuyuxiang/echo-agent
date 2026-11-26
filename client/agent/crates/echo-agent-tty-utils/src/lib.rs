//! Lightweight process-spawning utilities for TTY safety.
//!
//! When a TUI/pager/raw-mode terminal owns the parent process's controlling
//! TTY, every child process must be detached — otherwise it (or its
//! grandchildren: npm, git, pinentry, ssh-agent …) can open `/dev/tty`
//! directly and spew mouse escape codes, capability-probe replies, or
//! credential prompts onto the live screen.
//!
//! This crate provides the minimal surface needed to make subprocess spawning
//! safe. It is intentionally tiny so that **every** crate in the workspace
//! can depend on it without pulling in heavyweight libraries.
//!
//! # Usage
//!
//! ```rust,no_run
//! use echo_agent_tty_utils::{detach_command, pager_env};
//! use std::process::Stdio;
//!
//! let mut cmd = tokio::process::Command::new("git");
//! cmd.args(["status", "--porcelain"]);
//! detach_command(&mut cmd);
//! cmd.stdin(Stdio::null());
//! cmd.envs(pager_env());
//! ```
//!
//! For `std::process::Command`:
//!
//! ```rust,no_run
//! use echo_agent_tty_utils::{detach_std_command, pager_env};
//! use std::process::Stdio;
//!
//! let mut cmd = std::process::Command::new("git");
//! cmd.args(["log", "--oneline"]);
//! detach_std_command(&mut cmd);
//! cmd.stdin(Stdio::null());
//! cmd.envs(pager_env());
//! ```

// A panic on a teardown path leaks whatever it was about to free; tests panic freely.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use std::collections::HashMap;
use std::io;

mod child_wait;
pub use child_wait::{is_child_wait_identity_uncertain, spawn_child_reaper, wait_child_bounded};

mod process_resources;
pub use process_resources::{
    ProcessCpu, ProcessResources, process_memory_limit, process_start_time, sample_process_cpu,
    sample_process_memory, sample_process_resources,
};

mod process_scope;
pub use process_scope::{ProcessScope, global_process_scope};

/// How long a shell gets to forward a hangup to its jobs before it is killed.
pub const HANGUP_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

pub mod runtime;

// ---------------------------------------------------------------------------
// TTY detach — pre_exec building block
// ---------------------------------------------------------------------------

/// Detach from the controlling TTY by starting a new session.
///
/// This prevents spawned subprocesses from opening `/dev/tty` and competing
/// with the TUI for terminal input. `Stdio::null()` only redirects fd 0;
/// programs like `ssh`, `ssh-add`, and interactive shells (`zsh -i`) bypass
/// it by opening `/dev/tty` directly.
///
/// If `setsid()` fails with `EPERM` (the process is already a process group
/// leader), falls back to `setpgid(0, 0)` which still gives process-group
/// isolation even though the controlling terminal remains reachable.
///
/// # Safety
///
/// Must only be called inside a `pre_exec` hook (between `fork` and `exec`).
/// Both `setsid()` and `setpgid()` are async-signal-safe (POSIX).
#[cfg(unix)]
pub fn detach_from_tty() -> io::Result<()> {
    use nix::errno::Errno;
    use nix::unistd::{Pid, setpgid, setsid};

    match setsid() {
        Ok(_) => Ok(()),
        Err(Errno::EPERM) => {
            setpgid(Pid::from_raw(0), Pid::from_raw(0))
                .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
            Ok(())
        }
        Err(e) => Err(io::Error::from_raw_os_error(e as i32)),
    }
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
pub fn detach_from_tty() -> io::Result<()> {
    Ok(())
}

/// Reset `oom_score_adj` to 0. The score is inherited across `fork`, so a
/// protected parent would otherwise shield everything it spawns, leaving a
/// runaway command unkillable.
///
/// Best-effort: failing the spawn is worse than keeping the inherited score.
///
/// # Safety
///
/// Safe between `fork` and `exec`: raw `open`/`write`/`close` only, which are
/// async-signal-safe and allocation-free.
#[cfg(target_os = "linux")]
pub fn reset_oom_score_adj() -> io::Result<()> {
    const PATH: &[u8] = b"/proc/self/oom_score_adj\0";
    // SAFETY: `PATH` is a NUL-terminated `'static` literal and `value` is passed
    // with its own length; both pointers stay valid for the calls.
    unsafe {
        let fd = libc::open(PATH.as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Ok(());
        }
        let value = b"0\n";
        libc::write(fd, value.as_ptr().cast(), value.len());
        libc::close(fd);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn reset_oom_score_adj() -> io::Result<()> {
    Ok(())
}

/// Set by the launcher of the cyber tool server, which itself runs under a
/// protective (negative) `oom_score_adj`, so the commands it spawns stay
/// ordinary OOM candidates instead of inheriting that protection.
#[cfg(unix)]
pub const RESET_CHILD_OOM_ENV: &str = "ECHO_AGENT_TOOLS_RESET_CHILD_OOM";

/// Lower this process's `oom_score_adj` to -900 so the kernel OOM killer
/// prefers any ordinary child (score 0) while the server remains a last-resort
/// victim (unlike -1000, which would exempt it entirely and can leave a capped
/// cgroup with no eligible victim at all). Lowering the score needs
/// root/`CAP_SYS_RESOURCE`; the error distinguishes a real misconfiguration
/// (`PermissionDenied`) from an expected non-procfs environment (`NotFound`).
/// The score is inherited across `fork`, so a caller that gets `Ok` MUST arm
/// [`RESET_CHILD_OOM_ENV`] or its whole subtree inherits the protection.
#[cfg(target_os = "linux")]
pub fn protect_from_oom_kill() -> io::Result<()> {
    std::fs::write("/proc/self/oom_score_adj", "-900\n")
}

#[cfg(not(target_os = "linux"))]
pub fn protect_from_oom_kill() -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(unix)]
pub fn detach_from_tty_reset_oom() -> io::Result<()> {
    reset_oom_score_adj()?;
    detach_from_tty()
}

/// Must be called pre-fork, not from inside the hook: reading the environment
/// is not async-signal-safe.
#[cfg(unix)]
pub fn detach_pre_exec_hook() -> fn() -> io::Result<()> {
    if std::env::var_os(RESET_CHILD_OOM_ENV).is_some() {
        detach_from_tty_reset_oom
    } else {
        detach_from_tty
    }
}

// ---------------------------------------------------------------------------
// tokio::process::Command wrapper
// ---------------------------------------------------------------------------

/// Detach a `tokio::process::Command` from the parent's controlling TTY/console.
///
/// - Unix: `pre_exec` hook calling `setsid` (EPERM fallback: `setpgid`).
/// - Windows: `CREATE_NO_WINDOW`. Do NOT add `DETACHED_PROCESS` — it
///   breaks stdio pipe inheritance for grandchildren (`cmd.exe` → `node`).
pub fn detach_command(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // SAFETY: every hook `detach_pre_exec_hook` can return is async-signal-safe,
        // and the env read that selects one happens here, before fork.
        unsafe {
            cmd.pre_exec(detach_pre_exec_hook());
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.creation_flags(CREATE_NO_WINDOW.0);
    }
}

/// Configure a short-lived search child ([`detach_command`], no stdin) that is
/// killed and queued for reaping if its future is dropped (cancellation).
pub fn detach_search_command(cmd: &mut tokio::process::Command) {
    detach_command(cmd);
    // `null_stdio`, not `Stdio::null()`: the latter opens `/dev/null` by path
    // during spawn setup, so an unlinked device fails the spawn outright and
    // takes out search for the rest of the process's life. See `null_stdio`.
    cmd.stdin(null_stdio());
    cmd.kill_on_drop(true);
}

// ---------------------------------------------------------------------------
// Null stdio that outlives /dev/null
// ---------------------------------------------------------------------------

/// A null stdio handle for a child, taken from a descriptor opened once rather
/// than from the `/dev/null` path.
///
/// Use this instead of [`std::process::Stdio::null`] on every spawn path, for
/// stdin and for discarded stdout/stderr alike. `Stdio::null()` opens
/// `/dev/null` *by path*, in the parent, during spawn setup — so if anything
/// in the sandbox unlinks the device, `spawn` fails with `ENOENT` before
/// fork/exec and **every** process-spawning tool dies for the rest of the
/// process's life, while tools that only touch the filesystem keep working and
/// make it look like a workspace fault. Nothing recreates the device, and
/// recreating it would itself require spawning something. A root
/// `go build -o /dev/null` whose build fails is enough to trigger it: Go
/// removes its `-o` target on failure.
///
/// An already-open descriptor keeps working after its directory entry is gone,
/// so this opens `/dev/null` once, read-write, and hands out dups.
///
/// A process that starts *after* the deletion has nothing to open and falls
/// back to the read end of a pipe whose write end is already closed: reads see
/// immediate EOF, as with `/dev/null`. That fallback is correct for stdin only
/// — a child writing to it gets `EBADF` — but it is reachable solely once the
/// device is already gone, where the alternative is not spawning at all. If
/// even that fails it degrades to `Stdio::null()`, i.e. today's behaviour.
#[cfg(unix)]
pub fn null_stdio() -> std::process::Stdio {
    use std::sync::OnceLock;

    static NULL_FD: OnceLock<Option<std::os::fd::OwnedFd>> = OnceLock::new();

    let cached = NULL_FD
        .get_or_init(|| open_null_fd(std::path::Path::new("/dev/null")).or_else(eof_pipe_fd));
    match cached {
        // dup(2) per spawn: `Stdio` takes ownership, the cache keeps the original.
        Some(fd) => fd
            .try_clone()
            .map_or_else(|_| std::process::Stdio::null(), std::process::Stdio::from),
        None => std::process::Stdio::null(),
    }
}

/// Windows has no `/dev/null` path to lose; `Stdio::null()` is already a handle.
#[cfg(windows)]
pub fn null_stdio() -> std::process::Stdio {
    std::process::Stdio::null()
}

/// Read-only descriptor for `path`, or `None` if it cannot be opened.
///
/// Split out so the "the descriptor outlives the path" property can be tested
/// against an ordinary file, without a mount namespace or root.
#[cfg(unix)]
fn open_null_fd(path: &std::path::Path) -> Option<std::os::fd::OwnedFd> {
    // Read AND write, because one cached descriptor serves both directions:
    // stdin reads EOF from it, stdout/stderr discard into it. A read-only fd
    // would look fine until a child wrote to it — `write` on `O_RDONLY` fails
    // with `EBADF`, which turns a discarded diagnostic into a failed command
    // (and, in a shell, spills the text onto stdout, corrupting captured
    // output). This mirrors `Stdio::null()` itself, which opens `/dev/null`
    // readable for stdin and writable for stdout/stderr.
    //
    // `OpenOptions` sets CLOEXEC, so the cached fd is not inherited wholesale.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()
        .map(Into::into)
}

/// Read end of a pipe whose write end is already closed — a reader sees EOF at
/// once, which is what `/dev/null` gives a child's stdin.
///
/// The last resort for a process that came up with no `/dev/null` to cache.
///
/// Both ends are close-on-exec, atomically via `pipe2(O_CLOEXEC)` on Linux.
/// That matters more here than for an ordinary pipe: a concurrent `fork`/`exec`
/// landing between `pipe()` and `fcntl(F_SETFD)` could inherit the **write**
/// end, and a child holding it open means no reader of the cached read end ever
/// sees EOF — a child given it as stdin would block instead of starting
/// cleanly. Since the descriptor is cached for the process's lifetime, that
/// would be sticky, and a hang is a worse outcome than the `ENOENT` this
/// fallback exists to avoid. Mirrors `os_pipe` in echo-agent-tools' shell_state,
/// including the best-effort `fcntl` path where `pipe2` is unavailable.
#[cfg(unix)]
fn eof_pipe_fd() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let mut fds = [0 as libc::c_int; 2];

    // SAFETY: `fds` is a valid, writable two-element array for the call's duration.
    #[cfg(target_os = "linux")]
    let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    // SAFETY: same.
    #[cfg(not(target_os = "linux"))]
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if created != 0 {
        return None;
    }

    // SAFETY: the pipe call reported success, so both descriptors are open and unowned.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

    // Non-Linux has no atomic form: close the window as fast as possible and
    // accept it, as the sibling helper does.
    #[cfg(not(target_os = "linux"))]
    {
        use std::os::fd::AsRawFd;
        for fd in [read.as_raw_fd(), write.as_raw_fd()] {
            // SAFETY: both descriptors are live and owned here.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }

    // Closing the write end is what turns reads into an immediate EOF.
    drop(write);
    Some(read)
}

// ---------------------------------------------------------------------------
// std::process::Command wrapper
// ---------------------------------------------------------------------------

/// Detach a `std::process::Command` from the parent's controlling TTY/console.
///
/// This is the `std` counterpart of [`detach_command`] (which only works with
/// `tokio::process::Command`). Use this when you need to spawn via
/// `std::process::Command` — e.g. in synchronous code or `spawn_blocking`.
///
/// - Unix: `pre_exec` hook calling `setsid` (EPERM fallback: `setpgid`).
/// - Windows: `CREATE_NO_WINDOW`.
pub fn detach_std_command(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: every hook `detach_pre_exec_hook` can return is async-signal-safe,
        // and the env read that selects one happens here, before fork.
        unsafe {
            cmd.pre_exec(detach_pre_exec_hook());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.creation_flags(CREATE_NO_WINDOW.0);
    }
}

// ---------------------------------------------------------------------------
// Parent-death binding — Linux PR_SET_PDEATHSIG
// ---------------------------------------------------------------------------

/// The `pre_exec` body for [`kill_on_parent_death_std`]: arm `PR_SET_PDEATHSIG`
/// and close the classic pdeathsig race (parent died between `fork` and
/// `prctl`, so the signal will never fire) by comparing `getppid()` against
/// the pid captured at spawn time.
///
/// In debug builds this also enforces that the command was **armed on the
/// thread that spawns it**: pdeathsig binds to the death of the spawning
/// thread, so a cross-thread arm+spawn would silently bind the child to a
/// different thread's lifetime than the arming site reasoned about. The
/// guard returns `Err(EINVAL)` — surfaced by `spawn()` as an
/// `InvalidInput` error — rather than panicking, because this closure runs
/// post-fork where unwinding is not async-signal-safe;
/// `io::Error::from_raw_os_error` is allocation-free.
///
/// # Safety
///
/// Must only be called inside a `pre_exec` hook (between `fork` and `exec`):
/// it calls only async-signal-safe libc functions (`prctl`, `getppid`,
/// `_exit`) and its error paths build errors via `from_raw_os_error` /
/// `last_os_error` — never `io::Error::new`/`other`, which allocate. The
/// debug-only thread guard reads `std::thread::current().id()` from the
/// fork-copied TLS of the spawning thread; that handle is lazily created,
/// so in the (rare) case the spawning thread never materialized it this
/// can allocate — accepted for a debug-only misuse guard.
#[cfg(target_os = "linux")]
fn bind_to_parent_death(parent_pid: u32, armed_thread: std::thread::ThreadId) -> io::Result<()> {
    // Post-fork, TLS is a copy of the SPAWNING thread's, so this observes
    // which thread called `spawn()`.
    if cfg!(debug_assertions) && std::thread::current().id() != armed_thread {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: prctl(PR_SET_PDEATHSIG, …) only sets the calling process's
    // parent-death signal; it reads/writes no caller memory.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // Parent already gone (pdeathsig can no longer fire): exit instead of
    // orphaning. A reparented child sees a ppid different from the pid the
    // spawn site captured.
    // SAFETY: getppid/_exit are async-signal-safe and take no pointers.
    if unsafe { libc::getppid() } as u32 != parent_pid {
        unsafe { libc::_exit(0) };
    }
    Ok(())
}

/// Bind the child's lifetime to the spawning process: on Linux the kernel
/// delivers `SIGTERM` to the child when the parent dies
/// (`PR_SET_PDEATHSIG`), so helper processes cannot outlive a crashed or
/// killed echoagent and pile up on shared hosts. No-op on non-Linux platforms
/// (macOS and Windows have no pdeathsig equivalent).
///
/// **Caveat: pdeathsig binds to the death of the spawning *thread*, not
/// the process — arm and `spawn()` on a thread that lives as long as the
/// parent process.** Debug builds enforce arm-thread == spawn-thread: a
/// mismatch fails the `spawn()` with `InvalidInput` (`EINVAL`).
///
/// **Opt-in.** Only use this for helpers that are useless without their
/// parent (idle inhibitors, protocol children speaking over inherited
/// pipes). Never apply it to processes designed to outlive the client —
/// leader daemons, workspace servers, backgrounded user tasks.
///
/// Composable with [`detach_std_command`]: `pre_exec` hooks run in
/// registration order, and `setsid`/`setpgid` do not clear the parent-death
/// signal, so this can be applied before or after a `detach_*` helper.
///
/// Further Linux caveat: the kernel clears the setting across a
/// setuid/setcap `execve`.
pub fn kill_on_parent_death_std(cmd: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent_pid = std::process::id();
        let armed_thread = std::thread::current().id();
        // SAFETY: bind_to_parent_death calls only async-signal-safe libc
        // functions and builds errors without allocating (see its docs for
        // the debug-only TLS read). Satisfies the pre_exec contract.
        unsafe {
            cmd.pre_exec(move || bind_to_parent_death(parent_pid, armed_thread));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
    }
}

/// Bind the *current* process's lifetime to its parent: on Linux, arm
/// `PR_SET_PDEATHSIG(SIGTERM)` so this process is terminated when the
/// process that spawned it dies. No-op elsewhere.
///
/// This is the child-side variant of [`kill_on_parent_death_std`] for protocol
/// servers whose parents are not spawned from this workspace (IDE clients,
/// the agent SDKs, `echo-agent-desktop` all spawn `echoagent agent … stdio`): the
/// child arms the binding itself at startup instead of relying on every
/// external spawner to.
///
/// Unlike the spawn-time helper there is no ppid race check: a direct
/// parent at pid 1 is legitimate here (containers where the client is PID
/// 1), so an already-dead parent is indistinguishable from that case. The
/// caller's stdin-EOF handling covers the parent-died-before-arm race —
/// dead parent means closed pipes.
///
/// The binding keys off the death of the **parent's thread that spawned
/// this process** — a property of the spawner that the child can neither
/// inspect nor enforce (unlike [`kill_on_parent_death_std`], whose debug guard
/// runs in the spawner). External spawners that fork protocol children
/// from short-lived worker threads will see the signal early; for the
/// stdio entrypoints this is equivalent to the parent closing the pipes.
///
/// # Errors
///
/// Returns the `prctl` errno on Linux when the arm fails; the process then
/// keeps its previous lifetime semantics (stdin-EOF only), so callers
/// should log the failure. This crate stays logging-free by design —
/// surfacing the result is the observable seam. Always `Ok(())` on
/// non-Linux platforms (no-op).
///
/// **Opt-in.** Only call from entrypoints that are useless without the
/// process that spawned them (e.g. stdio transports over inherited pipes).
/// Never from daemons designed to outlive their spawner.
pub fn kill_current_process_on_parent_death() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_PDEATHSIG, …) only sets the calling process's
        // parent-death signal; it reads/writes no caller memory.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Process group lifecycle
// ---------------------------------------------------------------------------

/// Bound on waiting for an already-killed child (or its pipe readers) before
/// abandoning it.
///
/// A kill normally makes `child.wait()` resolve in milliseconds, but a child
/// wedged in an uninterruptible kernel syscall (D-state — e.g. a read on a
/// hard NFS mount whose server stopped responding) only observes the signal
/// when that syscall returns, which can be effectively never. Callers that
/// must not block (tool futures, turn loops) wait at most this long, then
/// abandon the corpse to the runtime's orphan reaper.
pub const KILL_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reap an already-killed child, waiting at most `bound` (usually
/// [`KILL_REAP_TIMEOUT`]).
///
/// Returns the exit status when the child was reaped in time. `None` covers
/// both failure shapes — the bound expired (see [`KILL_REAP_TIMEOUT`]) and
/// `wait()` itself erred (e.g. the child was already reaped elsewhere) — the
/// caller's obligation is identical in either case: the kill signal is
/// already delivered, there is no status to report, and the corpse is left
/// to tokio's orphan reaper. Callers should log the `None` case.
pub async fn reap_killed_bounded(
    child: &mut tokio::process::Child,
    bound: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    match tokio::time::timeout(bound, child.wait()).await {
        Ok(res) => res.ok(),
        Err(_elapsed) => None,
    }
}

/// True when `pid` is gone or a zombie awaiting reap — i.e. no longer running.
/// Test/assertion observation only — production liveness checks must use
/// [`ProcessGroup::has_live_members`], which counts zombies as live.
#[cfg(unix)]
pub fn process_not_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            // State is the first field after the parenthesized comm.
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .and_then(|rest| rest.trim_start().chars().next())
                .is_some_and(|state| state == 'Z'),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        match std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
        {
            // Can't tell; report "running" so callers fail loudly.
            Err(_) => false,
            Ok(out) => {
                let stat = String::from_utf8_lossy(&out.stdout);
                let stat = stat.trim();
                stat.is_empty() || stat.starts_with('Z')
            }
        }
    }
}

/// Configure a command so the spawned child becomes the leader of a new
/// process group.
pub fn new_process_group(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP.0);
    }
}

/// A Unix process-group id validated as safe to pass to `killpg`.
///
/// `killpg(pgid)` is `kill(-pgid)`, so a degenerate pgid escalates a scoped
/// group-kill into a broadcast: `0` signals the *caller's own* group, `1`
/// signals init, and the caller's own pgid would SIGKILL this very process and
/// its whole tree. [`ProcessGroupId::new`] rejects all three, so holding one is
/// a standing guarantee that `killpg` can only ever reach a real, foreign
/// group — the highest-blast-radius primitive in process teardown is validated
/// once, at enrollment, rather than re-checked at each call site.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessGroupId(u32);

#[cfg(unix)]
impl ProcessGroupId {
    /// Validate a group-leader pid. Errors for pid `0` (the caller's own
    /// group), pid `1` (init), or the caller's own process group (signalling it
    /// would kill this very process). A child spawned into its own group
    /// (`setpgid`/`setsid`, e.g. via [`new_process_group`] or a `detach_*`
    /// helper) always has a leader pid `> 1` distinct from the caller's pgid, so
    /// a well-formed enrollment never trips this — it only catches a child that
    /// was never grouped, which would otherwise broadcast the kill.
    pub fn new(pid: u32) -> io::Result<Self> {
        if pid <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing degenerate process-group id {pid} (0 = own group, 1 = init)"),
            ));
        }
        // killpg_unix casts `pid as i32`; values > i32::MAX wrap to negative,
        // and killpg with a negative pgid returns EINVAL on Linux/macOS. Reject
        // here so the invariant is safe-by-construction, not safe-by-OS-quirk.
        if pid > i32::MAX as u32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("process-group id {pid} exceeds i32::MAX; cannot be used with killpg"),
            ));
        }
        if i64::from(pid) == i64::from(nix::unistd::getpgrp().as_raw()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to killpg the caller's own process group ({pid})"),
            ));
        }
        Ok(Self(pid))
    }

    /// The validated raw process-group id.
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Process-tree teardown handle.
///
/// - Unix: holds the validated group-leader id ([`ProcessGroupId`]); dispatches
///   to `killpg(pgid, signal)`.
/// - Windows: holds a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
///
/// **Drop semantics differ by platform.** On Windows, drop terminates every
/// process in the job. On Unix, drop is a no-op — call [`kill`](Self::kill)
/// or [`terminate`](Self::terminate) explicitly.
pub struct ProcessGroup {
    /// `None` until a child is enrolled via [`Self::attach_pid`]; then `Some`
    /// holds a killpg-safe id. The pid is set once at attach and never
    /// auto-cleared — PID-reuse safety comes from the *owner* dropping this
    /// group (its `Arc`) at reap, after which nothing can `kill` it, not from
    /// this field resetting itself.
    #[cfg(unix)]
    leader: Option<ProcessGroupId>,
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
    /// Set for a shell that owns a terminal, whose job-control children only
    /// die if the shell is asked to hang up first.
    hangup_first: bool,
}

#[cfg(windows)]
unsafe impl Send for ProcessGroup {}
#[cfg(windows)]
unsafe impl Sync for ProcessGroup {}

impl ProcessGroup {
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                leader: None,
                hangup_first: false,
            })
        }
        #[cfg(windows)]
        {
            use std::mem::{size_of, zeroed};
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };
            use windows::core::PCWSTR;

            let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .map_err(|e| io::Error::other(format!("CreateJobObjectW: {e}")))?;

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let result = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if let Err(e) = result {
                let _ = unsafe { CloseHandle(job) };
                return Err(io::Error::other(format!("SetInformationJobObject: {e}")));
            }

            Ok(Self {
                job,
                hangup_first: false,
            })
        }
    }

    pub fn attach(&mut self, child: &tokio::process::Child) -> io::Result<()> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("ProcessGroup::attach: child has already exited"))?;
        self.attach_pid(pid)
    }

    /// Attach a `std::process::Child` (rather than tokio's). The process must be
    /// (or lead) its own group/job — e.g. spawned via [`new_process_group`] (Unix
    /// `setpgid`) or a `detach_*` helper (Unix `setsid`) — otherwise `kill`
    /// would signal the wrong group.
    ///
    /// Unix still goes through [`attach_pid`]. Windows uses the child's stable
    /// process handle (`AsHandle`) rather than `OpenProcess` by PID.
    pub fn attach_std(&mut self, child: &std::process::Child) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.attach_pid(child.id())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsHandle, AsRawHandle};
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::JobObjects::AssignProcessToJobObject;

            let process_handle = HANDLE(child.as_handle().as_raw_handle());
            // SAFETY: both handles are valid and borrowed for the duration of the call.
            unsafe { AssignProcessToJobObject(self.job, process_handle) }
                .map_err(|e| io::Error::other(format!("AssignProcessToJobObject: {e}")))
        }
    }

    /// Attach an already-spawned process by raw PID. The process must be (or
    /// lead) its own group/job — e.g. spawned via [`new_process_group`] (Unix
    /// `setpgid`) or a `detach_*` helper (Unix `setsid`) — otherwise `kill`
    /// would signal the wrong group.
    pub fn attach_pid(&mut self, pid: u32) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.leader = Some(ProcessGroupId::new(pid)?);
            Ok(())
        }
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::JobObjects::AssignProcessToJobObject;
            use windows::Win32::System::Threading::{
                OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            };

            let process_handle =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
                    .map_err(|e| io::Error::other(format!("OpenProcess({pid}): {e}")))?;

            let assign_result = unsafe { AssignProcessToJobObject(self.job, process_handle) };
            let _ = unsafe { CloseHandle(process_handle) };

            assign_result
                .map_err(|e| io::Error::other(format!("AssignProcessToJobObject({pid}): {e}")))
        }
    }

    pub fn terminate(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.killpg_unix(nix::sys::signal::Signal::SIGTERM)
        }
        #[cfg(windows)]
        {
            self.terminate_job(1)
        }
    }

    pub fn kill(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.killpg_unix(nix::sys::signal::Signal::SIGKILL)
        }
        #[cfg(windows)]
        {
            self.terminate_job(1)
        }
    }

    /// Whether any process still exists in this group. `None` where the
    /// platform cannot say (Windows, `EPERM`); treat it as alive.
    ///
    /// Over-reports, never under-reports: an unreaped zombie is still a
    /// process, so it counts as live. Filtering zombies out would let a
    /// reaped leader with a live descendant look empty.
    pub fn has_live_members(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let Some(leader) = self.leader else {
                return Some(false);
            };
            match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(leader.get() as i32), None) {
                Ok(()) => Some(true),
                Err(nix::errno::Errno::ESRCH) => Some(false),
                Err(_) => None,
            }
        }
        #[cfg(windows)]
        {
            None
        }
    }

    /// Ask an interactive shell to hang up. Its job-control children each live
    /// in their own process group, which no `killpg` here reaches, but a shell
    /// forwards the hangup to them before it exits.
    pub fn hangup(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.killpg_unix(nix::sys::signal::Signal::SIGHUP)?;
            // A stopped shell cannot forward the hangup until it resumes.
            self.killpg_unix(nix::sys::signal::Signal::SIGCONT)
        }
        #[cfg(windows)]
        {
            Ok(())
        }
    }

    /// Whether teardown should hang this group up before killing it. Never on
    /// Windows, where the hangup is a no-op and the Job Object takes the tree.
    pub fn wants_hangup(&self) -> bool {
        cfg!(unix) && self.hangup_first
    }

    /// Mark this group as a terminal-owning shell. See [`Self::hangup`].
    pub fn hang_up_before_kill(&mut self) {
        self.hangup_first = true;
    }

    #[cfg(unix)]
    fn killpg_unix(&self, signal: nix::sys::signal::Signal) -> io::Result<()> {
        // `leader` is `None` until a child is enrolled, and a `ProcessGroupId`
        // is killpg-safe by construction (never 0/1/own-group), so this can only
        // ever signal a real, foreign group.
        let Some(leader) = self.leader else {
            return Ok(());
        };
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(leader.get() as i32), signal)
            .map_err(|e| io::Error::from_raw_os_error(e as i32))
    }

    #[cfg(windows)]
    fn terminate_job(&self, exit_code: u32) -> io::Result<()> {
        use windows::Win32::System::JobObjects::TerminateJobObject;
        unsafe { TerminateJobObject(self.job, exit_code) }
            .map_err(|e| io::Error::other(format!("TerminateJobObject: {e}")))
    }
}

#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.job) };
    }
}

// ---------------------------------------------------------------------------
// Environment variable helpers
// ---------------------------------------------------------------------------

/// Returns environment variables that prevent CLI tools from launching any
/// interactive program that would block waiting for user input — pagers,
/// editors, credential prompts.
pub fn pager_env() -> HashMap<String, String> {
    HashMap::from([
        ("PAGER".to_string(), passthrough_pager().to_string()),
        ("GIT_PAGER".to_string(), passthrough_pager().to_string()),
        ("GH_PAGER".to_string(), passthrough_pager().to_string()),
        ("MANPAGER".to_string(), passthrough_pager().to_string()),
        ("AWS_PAGER".to_string(), String::new()),
        ("SYSTEMD_PAGER".to_string(), passthrough_pager().to_string()),
        ("GIT_EDITOR".to_string(), noop_cmd().to_string()),
        ("GIT_SEQUENCE_EDITOR".to_string(), noop_cmd().to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        // Empty (== unset for gpg) so gpg-agent/pinentry can't grab the TUI's tty; with setsid detach, signing fails cleanly instead of corrupting the screen.
        ("GPG_TTY".to_string(), String::new()),
    ])
}

/// Environment variables set on every git command to suppress interactive prompts.
pub const GIT_AUTH_SUPPRESSION_ENVS: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("GIT_LFS_SKIP_SMUDGE", "1"),
    ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
];

/// Git command with auth/LFS/SSH prompt suppression and `--no-optional-locks`.
///
/// Respects `GIT_BIN_PATH` for hermetic git in Bazel test sandboxes.
///
/// `--no-optional-locks` skips *optional* maintenance locks only (for example
/// `status` refreshing the index). Required locks for the requested operation
/// are still taken. Prefer this for readers (`status`, `cat-file`, `rev-parse`).
pub fn git_command() -> std::process::Command {
    let mut cmd = git_command_base();
    cmd.arg("--no-optional-locks");
    cmd
}

/// Like [`git_command`], but omits `--no-optional-locks`.
///
/// Writers such as `fetch` may take optional maintenance locks (packed-refs
/// refresh, etc.) in addition to required locks. Use this for mutating git
/// that should not skip those optional locks under concurrent restore.
pub fn git_command_locking() -> std::process::Command {
    git_command_base()
}

fn git_command_base() -> std::process::Command {
    let mut hermetic_exec_path: Option<std::path::PathBuf> = None;
    let git = match std::env::var("GIT_BIN_PATH") {
        Ok(p) => {
            let p = std::path::PathBuf::from(p);
            let p = if p.is_relative() {
                std::env::current_dir().unwrap_or_default().join(&p)
            } else {
                p
            };
            // git-minimal spawns subcommands (`git stash` → `git
            // update-index`) through its exec path, which is baked to a
            // build-machine prefix. Helpers live next to the binary, so point
            // the exec path there. Skip the host-fallback wrapper: host git
            // must keep its own exec path.
            if p.file_name().is_some_and(|name| name == "git") {
                hermetic_exec_path = p.parent().map(std::path::Path::to_path_buf);
            }
            p.to_string_lossy().into_owned()
        }
        Err(_) => "git".to_string(),
    };
    let mut cmd = std::process::Command::new(&git);
    detach_std_command(&mut cmd);
    cmd.stdin(std::process::Stdio::null());
    cmd.envs(pager_env());
    for &(key, val) in &GIT_AUTH_SUPPRESSION_ENVS {
        cmd.env(key, val);
    }
    if let Some(exec_path) = hermetic_exec_path {
        cmd.env("GIT_EXEC_PATH", exec_path);
    }
    cmd
}

fn passthrough_pager() -> &'static str {
    #[cfg(unix)]
    {
        "cat"
    }
    #[cfg(not(unix))]
    {
        ""
    }
}

fn noop_cmd() -> &'static str {
    #[cfg(unix)]
    {
        "true"
    }
    #[cfg(not(unix))]
    {
        r"C:\Windows\System32\cmd.exe /c exit 0"
    }
}

// ---------------------------------------------------------------------------
// Stderr redirection — shield TUI output from C-library noise
// ---------------------------------------------------------------------------

/// The dup'd stderr fd that writes to the real terminal. Set once by
/// [`redirect_native_stderr`]. Stored as [`OwnedFd`](std::os::unix::io::OwnedFd)
/// for type safety; the [`OnceLock`](std::sync::OnceLock) keeps it alive for the
/// entire process lifetime.
#[cfg(unix)]
static TUI_STDERR_FD: std::sync::OnceLock<std::os::unix::io::OwnedFd> = std::sync::OnceLock::new();

/// Redirect native stderr (fd 2) to `/dev/null` so that messages written
/// directly to fd 2 by C libraries do not interleave with TUI escape
/// sequences.
///
/// On macOS, `libsystem_malloc` writes `MallocStackLogging` diagnostics
/// directly to fd 2 from C code (bypassing Rust's `stderr()` lock) when
/// a `fork()`'d child process cannot turn off the parent's stack logging.
/// The message originates from `turn_off_stack_logging()` in Apple's
/// open-source `libmalloc`:
/// <https://opensource.apple.com/source/libmalloc/>
///
/// After this call, all intentional terminal output should go through
/// the `File` obtained from [`dup_tui_stderr`].
///
/// # Platform
///
/// Unix-only. On non-Unix platforms this is a no-op.
///
/// Must be called **very early**, before any threads are spawned, to avoid
/// racing with other code writing to fd 2. The TUI's `spawn_writer_thread`
/// should be called after this.
pub fn redirect_native_stderr() {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

        // SAFETY: dup(2) is safe on fd 2 (stderr), which is always valid
        // at process start.
        let duped = unsafe { libc::dup(2) };
        if duped < 0 {
            return; // best-effort; don't crash
        }

        // Store the duped fd (as OwnedFd) before redirecting.
        // SAFETY: `duped` is a freshly dup'd, valid file descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(duped) };
        let _ = TUI_STDERR_FD.set(owned);

        // Flush any buffered stderr data before redirecting fd 2 to
        // /dev/null, so nothing is silently lost.
        let _ = std::io::stderr().flush();

        // Open /dev/null and dup2 it onto fd 2.
        let devnull = match std::fs::OpenOptions::new().write(true).open("/dev/null") {
            Ok(f) => f,
            Err(_) => return,
        };
        // SAFETY: devnull.as_raw_fd() is valid; fd 2 is a valid target.
        unsafe { libc::dup2(devnull.as_raw_fd(), 2) };
        // devnull File is dropped here; fd 2 now points at /dev/null.
    }
}

/// Create a new owned `File` that writes to the real terminal stderr.
///
/// This calls `dup(2)` on the saved fd to create an independently-owned
/// file descriptor. Each caller gets their own fd that they can wrap in
/// a `BufWriter`, pass to a thread, etc. Dropping the returned `File`
/// closes only that caller's dup'd copy — the underlying terminal fd
/// is never affected.
///
/// If [`redirect_native_stderr`] was not called, this dups normal fd 2.
pub fn dup_tui_stderr() -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let source_fd = TUI_STDERR_FD.get().map(|fd| fd.as_raw_fd()).unwrap_or(2);
        // SAFETY: source_fd is either the dup'd stderr (valid for the
        // process lifetime via OnceLock<OwnedFd>) or 2 (normal stderr).
        // dup() returns a new independently-owned fd.
        let new_fd = unsafe { libc::dup(source_fd) };
        if new_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: new_fd is a freshly dup'd, owned file descriptor.
        Ok(unsafe { std::fs::File::from_raw_fd(new_fd) })
    }
    #[cfg(not(unix))]
    {
        // On Windows, `redirect_native_stderr` is a no-op, so fd 2 is
        // always the real stderr. We use `try_clone()` on a temporarily
        // created File to get an independently-owned handle via
        // `DuplicateHandle` — avoiding the `from_raw_handle` footgun
        // where `File` would take ownership of the process stderr handle
        // and close it on drop.
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        let stderr_handle = unsafe {
            windows::Win32::System::Console::GetStdHandle(
                windows::Win32::System::Console::STD_ERROR_HANDLE,
            )
            .map_err(|e| io::Error::other(format!("GetStdHandle: {e}")))?
        };
        // Create a temporary File from the raw handle, clone it (which
        // calls DuplicateHandle internally), then forget the original so
        // it doesn't close the process stderr handle.
        let temp = unsafe { std::fs::File::from_raw_handle(stderr_handle.0 as _) };
        let cloned = temp.try_clone();
        // Prevent `temp` from closing the process stderr handle.
        std::mem::forget(temp);
        cloned
    }
}

/// Restore fd 2 to point to the real terminal stderr.
///
/// Call this before exiting so that any final messages (panic output,
/// tracing flushes) reach the user's terminal rather than `/dev/null`.
pub fn restore_native_stderr() {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(fd) = TUI_STDERR_FD.get() {
            // SAFETY: fd is the dup'd stderr (valid for process lifetime
            // via OnceLock<OwnedFd>). dup2 atomically replaces fd 2 with
            // a copy of fd.
            unsafe { libc::dup2(fd.as_raw_fd(), 2) };
        }
    }
}

/// Returns `true` when running inside Windows Subsystem for Linux (WSL1/WSL2).
/// Cached for process lifetime. Detection: the `WSL_DISTRO_NAME` / `WSL_INTEROP`
/// env vars, with a `/proc/sys/kernel/osrelease` `microsoft`/`wsl` substring
/// fallback that also catches WSL1 and env-stripped shells.
pub fn is_wsl() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(detect_is_wsl)
}

fn detect_is_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let env: HashMap<String, String> = std::env::vars().collect();
    let osrelease = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    is_wsl_from_inputs(&env, osrelease.as_deref())
}

/// Pure helper so tests can drive the env and `/proc` contents directly.
fn is_wsl_from_inputs(env: &HashMap<String, String>, osrelease: Option<&str>) -> bool {
    if env.contains_key("WSL_DISTRO_NAME") || env.contains_key("WSL_INTEROP") {
        return true;
    }
    osrelease.is_some_and(|s| {
        let lower = s.to_ascii_lowercase();
        lower.contains("microsoft") || lower.contains("wsl")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Serializes process-global `/proc/self/oom_score_adj` mutation across tests.
    #[cfg(target_os = "linux")]
    static TEST_OOM_SCORE_ADJ_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detach_command_does_not_panic() {
        let mut cmd = tokio::process::Command::new("echo");
        detach_command(&mut cmd);
    }

    /// Pins the mechanism every search-tool spawn site relies on: the child is
    /// detached into its own process group and killed when its `Child` drops.
    #[cfg(unix)]
    #[test]
    fn detach_search_command_kills_child_on_drop() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut cmd = tokio::process::Command::new("/bin/sleep");
            cmd.arg("30");
            detach_search_command(&mut cmd);
            #[allow(clippy::disallowed_methods)] // test child, killed on drop below
            let child = cmd.spawn().expect("spawn sleep");
            let pid = child.id().expect("child pid");

            // The pre_exec setsid lands between fork and exec; poll until the
            // child leaves our process group (pins the detach property).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid as i32)))
                .expect("getpgid")
                == nix::unistd::getpgrp()
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "sleep (pid {pid}) stayed in our process group — not detached"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            drop(child);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !process_not_running(pid) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "sleep (pid {pid}) still running 5s after its Child was dropped — leaked"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    /// `None` when lowering our own score below 0 is not permitted (it needs
    /// `CAP_SYS_RESOURCE`).
    #[cfg(target_os = "linux")]
    fn child_oom_score_under(hook: fn() -> io::Result<()>) -> Option<String> {
        // Hold for the full lower/spawn/restore window so parallel cargo test
        // cannot interleave sibling score mutations (same pattern as workspace-daemon).
        let _score_guard = TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let own = std::fs::read_to_string("/proc/self/oom_score_adj").expect("read own score");
        let restore = own.trim().to_owned();
        if std::fs::write("/proc/self/oom_score_adj", b"-500\n").is_err() {
            return None;
        }
        let mut cmd = std::process::Command::new("cat");
        cmd.arg("/proc/self/oom_score_adj")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped());
        // SAFETY: both hooks call only async-signal-safe primitives.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(hook);
        }
        let out = cmd.output().expect("spawn child");
        let _ = std::fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detach_from_tty_reset_oom_resets_the_child() {
        let _env = OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(score) = child_oom_score_under(detach_from_tty_reset_oom) else {
            return;
        };
        assert_eq!(
            score, "0",
            "the reset variant must zero the child's oom_score_adj"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn plain_detach_from_tty_leaves_child_oom_score_inherited() {
        let _env = OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(score) = child_oom_score_under(detach_from_tty) else {
            return;
        };
        assert_eq!(
            score, "-500",
            "plain detach_from_tty must not touch the child's inherited oom_score_adj (prod path)"
        );
    }

    /// Serializes every test that touches the process-global OOM state:
    /// [`RESET_CHILD_OOM_ENV`] and `/proc/self/oom_score_adj` (mutated by both
    /// [`child_oom_score_under`] and `protect_from_oom_kill`). Under parallel
    /// `cargo test` these must not interleave.
    #[cfg(target_os = "linux")]
    static OOM_SCORE_ADJ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(target_os = "linux")]
    #[test]
    fn detach_pre_exec_hook_defaults_to_no_reset() {
        let _env = OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(score) = child_oom_score_under(detach_pre_exec_hook()) else {
            return;
        };
        assert_eq!(
            score, "-500",
            "without the opt-in env the hook must not reset children"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detach_pre_exec_hook_selects_reset_when_env_armed() {
        let _env = OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: test-process env mutation, serialized with the sibling test
        // by the lock above; the var is removed before returning.
        unsafe { std::env::set_var(RESET_CHILD_OOM_ENV, "1") };
        let hook = detach_pre_exec_hook();
        unsafe { std::env::remove_var(RESET_CHILD_OOM_ENV) };
        let Some(score) = child_oom_score_under(hook) else {
            return;
        };
        assert_eq!(
            score, "0",
            "with the env armed the hook must reset the child's oom_score_adj"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protect_from_oom_kill_lowers_own_score_when_privileged() {
        let _env = OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let own = std::fs::read_to_string("/proc/self/oom_score_adj").expect("read own score");
        let restore = own.trim().to_owned();
        // Unprivileged (no CAP_SYS_RESOURCE) must surface as an error, never
        // a silent no-op.
        match protect_from_oom_kill() {
            Ok(()) => {
                let score =
                    std::fs::read_to_string("/proc/self/oom_score_adj").expect("read own score");
                assert_eq!("-900", score.trim());
                let _ = std::fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
            }
            Err(e) => assert_eq!(io::ErrorKind::PermissionDenied, e.kind()),
        }
    }

    #[test]
    fn detach_std_command_does_not_panic() {
        let mut cmd = std::process::Command::new("echo");
        detach_std_command(&mut cmd);
    }

    #[test]
    fn git_command_locking_matches_reader_except_optional_locks_flag() {
        use std::collections::HashMap;
        use std::ffi::{OsStr, OsString};

        let locking = git_command_locking();
        let reading = git_command();
        assert_eq!(locking.get_program(), reading.get_program());

        let lock_args: Vec<OsString> = locking.get_args().map(OsStr::to_os_string).collect();
        let read_args: Vec<OsString> = reading.get_args().map(OsStr::to_os_string).collect();
        assert_eq!(
            read_args.last().map(OsString::as_os_str),
            Some(OsStr::new("--no-optional-locks"))
        );
        assert_eq!(
            &read_args[..read_args.len().saturating_sub(1)],
            lock_args.as_slice()
        );

        let lock_envs: HashMap<_, _> = locking.get_envs().collect();
        let read_envs: HashMap<_, _> = reading.get_envs().collect();
        assert_eq!(lock_envs, read_envs);
    }

    #[test]
    fn new_process_group_does_not_panic() {
        let mut cmd = tokio::process::Command::new("echo");
        new_process_group(&mut cmd);
    }

    #[test]
    fn kill_on_parent_death_std_does_not_panic() {
        let mut cmd = std::process::Command::new("echo");
        kill_on_parent_death_std(&mut cmd);
    }

    #[cfg(unix)]
    #[test]
    fn has_live_members_tracks_the_group_emptying() {
        let mut group = ProcessGroup::new().expect("group");
        assert_eq!(group.has_live_members(), Some(false));

        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("1000")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach_std_command(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test: exercises ProcessGroup directly
        let mut child = cmd.spawn().expect("spawn sleeper");
        group.attach_std(&child).expect("attach");
        assert_eq!(group.has_live_members(), Some(true));

        group.kill().expect("kill group");
        // Required: an unreaped zombie still reports live.
        child.wait().expect("reap sleeper");
        assert_eq!(group.has_live_members(), Some(false));
    }

    /// Debug builds enforce the top-of-doc caveat that arming and spawning
    /// happen on the same (long-lived) thread — pdeathsig binds to the
    /// spawning thread's lifetime, so a cross-thread arm+spawn must fail
    /// the spawn with `InvalidInput` (`EINVAL` from the pre_exec guard)
    /// instead of silently binding to the wrong thread. The same-thread
    /// happy path is covered by `armed_child_survives_while_parent_lives`.
    #[cfg(all(target_os = "linux", debug_assertions))]
    #[test]
    fn cross_thread_arming_fails_spawn_in_debug_builds() {
        let mut cmd = std::thread::spawn(|| {
            let mut cmd = std::process::Command::new("true");
            kill_on_parent_death_std(&mut cmd);
            cmd
        })
        .join()
        .expect("arming thread");
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let error = cmd
            .spawn()
            .expect_err("cross-thread arm+spawn must fail in debug builds");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidInput,
            "expected the EINVAL thread guard, got: {error}"
        );
    }

    // ── parent-death binding integration tests (Linux) ──────────
    //
    // The scenario needs a real intermediate parent process, so the test
    // binary re-execs itself (the `stderr_redirect_roundtrip_subprocess`
    // pattern): the driver spawns `pdeathsig_intermediate_entry`, which
    // spawns a long-sleeping grandchild armed with the helper and exits;
    // the driver then asserts the grandchild dies with it.

    /// Env marker dispatching the re-exec'd test binary into the
    /// intermediate-parent logic.
    #[cfg(target_os = "linux")]
    const PDEATHSIG_INTERMEDIATE_ENV: &str = "__ECHO_AGENT_TTY_UTILS_PDEATHSIG_INTERMEDIATE";

    /// Intermediate parent: spawn the armed grandchild, report its pid on
    /// stdout, linger briefly so the driver can observe it alive, then exit
    /// (which must take the grandchild down via pdeathsig).
    #[cfg(target_os = "linux")]
    #[test]
    fn pdeathsig_intermediate_entry() {
        if std::env::var_os(PDEATHSIG_INTERMEDIATE_ENV).is_none() {
            return; // skip when not invoked as the re-exec'd intermediate
        }

        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("300")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Composability under test: detach (setsid) plus parent-death
        // binding on the same command, in the documented order.
        detach_std_command(&mut cmd);
        kill_on_parent_death_std(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let child = cmd.spawn().expect("spawn armed grandchild");
        println!("grandchild:{}", child.id());
        // Do not reap: the grandchild must outlive this handle and die only
        // via pdeathsig when this process exits.
        std::mem::forget(child);
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }

    /// A child spawned with [`kill_on_parent_death_std`] must not outlive
    /// its parent: the kernel SIGTERMs it when the parent exits.
    #[cfg(target_os = "linux")]
    #[test]
    fn armed_child_dies_when_parent_exits() {
        let exe = std::env::current_exe().expect("current_exe");
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--exact")
            .arg("tests::pdeathsig_intermediate_entry")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(PDEATHSIG_INTERMEDIATE_ENV, "1")
            // The intermediate is a fresh libtest run of exactly one filtered
            // test. Strip Bazel's per-shard env so a `shard_count` build can't
            // partition that single test into another shard (running zero
            // tests), and drop any inherited filter.
            .env_remove("TEST_SHARD_INDEX")
            .env_remove("TEST_TOTAL_SHARDS")
            .env_remove("TEST_SHARD_STATUS_FILE")
            .env_remove("TESTBRIDGE_TEST_ONLY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut intermediate = cmd.spawn().expect("spawn intermediate test process");

        // Grandchild pid from the intermediate's stdout. Substring-match, not
        // line-prefix parsing: with `--nocapture` libtest prints the
        // `test tests::… ... ` header WITHOUT a trailing newline, so the
        // reported pid shares its line with harness chrome.
        let stdout = intermediate.stdout.take().expect("piped stdout");
        let mut reader = std::io::BufReader::new(stdout);
        let mut seen: Vec<String> = Vec::new();
        let grandchild_pid = loop {
            use std::io::BufRead as _;
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .expect("read intermediate stdout");
            assert_ne!(
                n, 0,
                "intermediate exited without reporting a grandchild; stdout seen: {seen:?}"
            );
            if let Some(idx) = line.find("grandchild:") {
                let digits: String = line[idx + "grandchild:".len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                break digits.parse::<i32>().unwrap_or_else(|e| {
                    panic!("parse grandchild pid from {line:?}: {e}");
                });
            }
            seen.push(line);
        };

        // `kill(pid, 0)` succeeds on zombies, and under a non-reaping
        // subreaper (e.g. a test process-wrapper) the orphaned grandchild can
        // linger as a zombie after the SIGTERM. Treat zombie as dead: the
        // signal did its job.
        let alive = |pid: i32| match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => false,
            Ok(stat) => {
                // Field 3 (state) is the first token after the last ')' —
                // comm can itself contain ')'.
                let state = stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.trim_start().chars().next());
                state != Some('Z')
            }
        };
        assert!(
            alive(grandchild_pid),
            "grandchild should be running while its parent is alive"
        );

        let status = intermediate.wait().expect("wait intermediate");
        assert!(status.success(), "intermediate test run failed: {status:?}");

        // Parent gone — the armed grandchild must be SIGTERMed by the kernel
        // (orphan → reparent → reap → ESRCH). Poll with a deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while alive(grandchild_pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            !alive(grandchild_pid),
            "grandchild pid {grandchild_pid} outlived its parent despite \
             kill_on_parent_death_std (PR_SET_PDEATHSIG not effective)"
        );
    }

    /// The binding must be one-directional: a live parent keeps its armed
    /// child alive (no false-positive from the ppid race check or from
    /// composing with `detach_std_command`).
    #[cfg(target_os = "linux")]
    #[test]
    fn armed_child_survives_while_parent_lives() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach_std_command(&mut cmd);
        kill_on_parent_death_std(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut child = cmd.spawn().expect("spawn armed child");

        // The binding must not kill a child whose parent (this process) is
        // alive and whose ppid matches the captured pid.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "armed child died even though its parent is still alive"
        );
        child.kill().expect("kill child");
        child.wait().expect("reap child");
    }

    fn wsl_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn is_wsl_detects_wsl_distro_name() {
        assert!(is_wsl_from_inputs(
            &wsl_env(&[("WSL_DISTRO_NAME", "Ubuntu")]),
            None
        ));
    }

    #[test]
    fn is_wsl_detects_wsl_interop() {
        assert!(is_wsl_from_inputs(
            &wsl_env(&[("WSL_INTEROP", "/run/WSL/8_interop")]),
            None,
        ));
    }

    // osrelease fallback also catches env-stripped shells.
    #[test]
    fn is_wsl_detects_wsl2_osrelease() {
        assert!(is_wsl_from_inputs(
            &wsl_env(&[]),
            Some("5.15.167.4-microsoft-standard-WSL2\n"),
        ));
    }

    #[test]
    fn is_wsl_detects_wsl1_osrelease() {
        assert!(is_wsl_from_inputs(
            &wsl_env(&[]),
            Some("4.4.0-19041-Microsoft\n")
        ));
    }

    #[test]
    fn is_wsl_osrelease_match_is_case_insensitive() {
        assert!(is_wsl_from_inputs(
            &wsl_env(&[]),
            Some("WSLg-experimental\n")
        ));
        assert!(is_wsl_from_inputs(
            &wsl_env(&[]),
            Some("Foo-MICROSOFT-bar\n")
        ));
    }

    #[test]
    fn is_wsl_rejects_native_linux() {
        assert!(!is_wsl_from_inputs(
            &wsl_env(&[]),
            Some("6.5.0-21-generic\n")
        ));
    }

    #[test]
    fn is_wsl_rejects_when_osrelease_missing() {
        assert!(!is_wsl_from_inputs(&wsl_env(&[]), None));
    }

    #[test]
    fn is_wsl_env_var_match_is_exact_key_not_substring() {
        // A var whose name merely contains "WSL" must not trigger detection.
        assert!(!is_wsl_from_inputs(
            &wsl_env(&[("MY_WSLISH_VAR", "1"), ("OTHER", "wsl_in_value")]),
            Some("6.5.0-21-generic\n"),
        ));
    }

    #[test]
    fn pager_env_has_expected_keys() {
        let env = pager_env();
        assert!(env.contains_key("PAGER"));
        assert!(env.contains_key("GIT_PAGER"));
        assert!(env.contains_key("GH_PAGER"));
        assert!(env.contains_key("GIT_TERMINAL_PROMPT"));
        // Present and empty so gpg/pinentry cannot target the TUI tty.
        assert_eq!(env.get("GPG_TTY"), Some(&String::new()));
    }

    // ── stderr redirect integration tests ────────────────────────
    //
    // The redirect/dup/restore cycle mutates process-global state
    // (fd 2 and a `OnceLock`), so the full flow runs in a subprocess
    // to avoid polluting other tests.

    /// `dup_tui_stderr` returns a writable File even without prior redirect.
    #[test]
    fn dup_tui_stderr_without_redirect_returns_writable_file() {
        use std::io::Write;
        let mut f = dup_tui_stderr().expect("dup_tui_stderr should succeed");
        f.write_all(b"").expect("zero-length write should succeed");
    }

    /// The `File` returned by `dup_tui_stderr` can write multi-byte UTF-8
    /// without truncation or byte-level corruption.
    ///
    /// On Windows, a `File` wrapping a console handle uses `WriteFile`
    /// (byte-oriented, code-page-dependent) instead of `WriteConsoleW`
    /// (UTF-16, code-page-independent). This test verifies the bytes are
    /// at least accepted without error. Full Unicode correctness on Windows
    /// additionally requires `SetConsoleOutputCP(65001)` or using
    /// `std::io::stderr()` (which routes through `WriteConsoleW`).
    #[test]
    fn dup_tui_stderr_accepts_multibyte_utf8() {
        use std::io::Write;
        let mut f = dup_tui_stderr().expect("dup_tui_stderr should succeed");
        // Braille (3 bytes each), Powerline icon (3 bytes), emoji (4 bytes).
        let payload = "⣀⣾⠿⠛\u{e0a0}\u{1F600}";
        f.write_all(payload.as_bytes())
            .expect("multi-byte UTF-8 write should succeed");
        f.flush().expect("flush should succeed");
    }

    /// Spawn a subprocess that exercises redirect → dup → restore.
    #[cfg(unix)]
    #[test]
    fn stderr_redirect_roundtrip_subprocess() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::stderr_redirect_roundtrip_body")
            .env("__ECHO_AGENT_STDERR_REDIRECT_SUBPROCESS", "1")
            .status()
            .expect("failed to spawn test subprocess");
        assert!(status.success(), "subprocess integration test failed");
    }

    /// The actual redirect/dup/restore test body (only runs as subprocess).
    #[cfg(unix)]
    #[test]
    fn stderr_redirect_roundtrip_body() {
        if std::env::var("__ECHO_AGENT_STDERR_REDIRECT_SUBPROCESS").is_err() {
            return; // skip when not invoked as subprocess
        }

        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        // 1. After redirect, fd 2 points at /dev/null.
        redirect_native_stderr();
        let fd2_flags = unsafe { libc::fcntl(2, libc::F_GETFD) };
        assert!(fd2_flags >= 0, "fd 2 should be valid after redirect");

        // Verify fd 2 is /dev/null by checking that writes succeed
        // but reading the inode shows it's a different file.
        let mut stat_fd2: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(2, &mut stat_fd2) };
        assert_eq!(r, 0, "fstat(2) should succeed");

        // 2. dup_tui_stderr returns a working fd to the real terminal.
        let mut tui = dup_tui_stderr().expect("dup_tui_stderr after redirect");
        assert!(tui.as_raw_fd() > 2, "dup'd fd should be > 2");
        tui.write_all(b"").expect("writing to dup'd fd should work");

        // 3. A second call returns an independent fd.
        let tui2 = dup_tui_stderr().expect("second dup_tui_stderr");
        assert_ne!(
            tui.as_raw_fd(),
            tui2.as_raw_fd(),
            "each call should return a distinct fd"
        );
        drop(tui2);
        // First fd still works after second is dropped.
        tui.write_all(b"").expect("first fd survives second drop");

        // 4. restore_native_stderr brings fd 2 back.
        restore_native_stderr();
        let fd2_after = unsafe { libc::fcntl(2, libc::F_GETFD) };
        assert!(fd2_after >= 0, "fd 2 should be valid after restore");

        // Verify fd 2 is no longer /dev/null (inode changed).
        let mut stat_after: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(2, &mut stat_after) };
        assert_eq!(r, 0, "fstat(2) after restore should succeed");
        assert_ne!(
            stat_fd2.st_ino, stat_after.st_ino,
            "fd 2 inode should differ after restore (no longer /dev/null)"
        );
    }

    #[tokio::test]
    async fn process_group_kill_terminates_attached_child() {
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", "ping", "-n", "60", "127.0.0.1"]);
            c
        } else {
            let mut c = tokio::process::Command::new("sleep");
            c.arg("60");
            c
        };
        new_process_group(&mut cmd);

        let mut group = ProcessGroup::new().expect("create ProcessGroup");
        #[allow(clippy::disallowed_methods)] // test: exercises ProcessGroup directly
        let mut child = cmd.spawn().expect("spawn child");
        group.attach(&child).expect("attach child to group");

        group.kill().expect("kill ProcessGroup");

        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("child should exit within 5s of group kill")
            .expect("wait returns ok");
        assert!(
            !status.success(),
            "killed child should not exit successfully"
        );
    }

    /// `kill()` must reap the WHOLE process group — including a GRANDCHILD the
    /// leader forks into the same group — not just the immediate leader. This is
    /// the `killpg` tree-kill property the LSP / MCP / terminal teardown relies
    /// on; the pre-fix code signalled only the direct child, orphaning
    /// grandchildren (e.g. a language server's own subprocesses).
    #[cfg(unix)]
    #[tokio::test]
    async fn process_group_kill_reaps_grandchild_tree() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        // Leader = sh; it backgrounds a grandchild in the SAME group (sh does
        // not `setsid` its children), prints the grandchild PID, then waits.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 60 & echo $!; wait"]);
        cmd.stdout(std::process::Stdio::piped());
        new_process_group(&mut cmd);

        let mut group = ProcessGroup::new().expect("create ProcessGroup");
        #[allow(clippy::disallowed_methods)] // test: exercises ProcessGroup + grandchild kill
        let mut child = cmd.spawn().expect("spawn leader");
        group.attach(&child).expect("attach leader to group");

        // Grandchild PID from the leader's stdout.
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .await
            .expect("read grandchild pid");
        let gc_pid: i32 = line.trim().parse().expect("parse grandchild pid");

        let alive =
            |pid: i32| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
        assert!(
            alive(gc_pid),
            "grandchild should be running before the kill"
        );

        group.kill().expect("kill ProcessGroup");

        // Leader exits.
        tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("leader should exit within 5s of group kill")
            .expect("wait ok");

        // The grandchild (same group) must ALSO be reaped by the killpg — poll
        // until gone (orphan → reparented to init → reaped → ESRCH).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while alive(gc_pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !alive(gc_pid),
            "killpg must reap the WHOLE group incl. the grandchild (tree-kill), not just the \
             leader; grandchild pid {gc_pid} still alive after group kill"
        );
    }

    /// The whole point of the cached descriptor: it keeps working after the
    /// path it came from is unlinked, which is what `Stdio::null()` cannot do.
    /// An ordinary file stands in for `/dev/null` so this needs no privileges.
    #[cfg(unix)]
    #[test]
    fn null_fd_outlives_its_path() {
        let path =
            std::env::temp_dir().join(format!("echo-agent-tty-utils-null-{}", std::process::id()));
        std::fs::write(&path, b"").expect("create stand-in null");
        let fd = open_null_fd(&path).expect("open stand-in null");
        std::fs::remove_file(&path).expect("unlink stand-in null");

        assert!(
            std::fs::File::open(&path).is_err(),
            "the path must be gone, or the test proves nothing"
        );
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(std::process::Stdio::from(fd.try_clone().expect("dup")))
            .status()
            .expect("spawn with a descriptor whose path was unlinked must still work");
        assert!(status.success());
    }

    /// The fallback descriptor must be close-on-exec, or a concurrently
    /// spawned child inherits the pipe and the cached read end never reaches
    /// EOF — a hang, and a sticky one, since the fd is cached for the process's
    /// lifetime. Also checks it reads EOF, which is the point of the fallback.
    #[cfg(unix)]
    #[test]
    fn eof_pipe_fd_is_cloexec_and_reads_eof() {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let fd = eof_pipe_fd().expect("pipe fallback");
        // SAFETY: `fd` is a live descriptor owned by this test.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD failed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "the cached fallback fd must be close-on-exec"
        );

        let mut buf = [0u8; 1];
        let read = std::fs::File::from(fd).read(&mut buf).expect("read");
        assert_eq!(read, 0, "the write end is closed, so reads must see EOF");
    }

    /// One cached descriptor serves both directions, so a child must be able
    /// to WRITE to it as well as read EOF from it.
    ///
    /// A read-only fd passes every stdin test and then fails here: `write` on
    /// `O_RDONLY` returns `EBADF`, so a discarded diagnostic becomes a failed
    /// command — and a shell spills the text onto stdout, corrupting output
    /// that callers parse.
    #[cfg(unix)]
    #[test]
    fn null_stdio_accepts_child_writes() {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", "echo diagnostic >&2; echo rc=$?"])
            .stdin(null_stdio())
            .stderr(null_stdio())
            .output()
            .expect("spawn");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("rc=0"),
            "writing to a nulled stderr must succeed, got {stdout:?}"
        );
        assert!(
            !stdout.contains("diagnostic"),
            "stderr text must be discarded, not spilled onto stdout: {stdout:?}"
        );
    }

    /// The real thing: `/dev/null` genuinely removed, in a throwaway mount
    /// namespace so only this process tree sees it gone.
    ///
    /// Covers both orders, because they exercise different halves of the fix:
    ///
    /// * `deleted-midway` — the production sequence. The device exists when the
    ///   server boots (so the descriptor is cached), then the sandbox loses it.
    ///   Asserts the control too: `Stdio::null()` must fail with `ENOENT` here,
    ///   or the test is not reproducing the bug it guards against.
    /// * `never-existed` — a process that comes up with no device at all, which
    ///   has no descriptor to cache and must reach the pipe fallback.
    ///
    /// Ignored by default: needs Linux with unprivileged user namespaces. Run
    /// with `cargo test -p echo-agent-tty-utils -- --ignored --nocapture`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs Linux + unprivileged user namespaces (unshare -Urm)"]
    fn null_stdio_survives_a_deleted_dev_null() {
        const PROBE_VAR: &str = "ECHO_AGENT_TTY_UTILS_DEV_NULL_PROBE";

        fn spawn_with(stdin: std::process::Stdio) -> std::io::Result<std::process::ExitStatus> {
            std::process::Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .stdin(stdin)
                .status()
        }

        // Inner half: re-entered inside the namespace.
        match std::env::var(PROBE_VAR).ok().as_deref() {
            Some("deleted-midway") => {
                // A regular file stands in for the device: creatable on the
                // namespace's tmpfs without privileges, and indistinguishable
                // for this purpose (an fd is an fd).
                assert!(
                    std::path::Path::new("/dev/null").exists(),
                    "setup: stand-in missing"
                );
                assert!(
                    spawn_with(null_stdio()).expect("prime").success(),
                    "priming spawn must succeed while the device is present"
                );

                std::fs::remove_file("/dev/null").expect("delete the device mid-run");

                let control = spawn_with(std::process::Stdio::null());
                assert!(
                    matches!(&control, Err(e) if e.kind() == std::io::ErrorKind::NotFound),
                    "control: Stdio::null() must fail with ENOENT once the device is gone, got {control:?}"
                );
                for i in 1..=3 {
                    assert!(
                        spawn_with(null_stdio())
                            .expect("spawn after deletion")
                            .success(),
                        "null_stdio() spawn #{i} must still work"
                    );
                }
                return;
            }
            Some("never-existed") => {
                assert!(
                    !std::path::Path::new("/dev/null").exists(),
                    "setup: device present"
                );
                assert!(
                    spawn_with(null_stdio())
                        .expect("pipe fallback must spawn")
                        .success(),
                    "a process that starts without /dev/null must still spawn"
                );
                return;
            }
            _ => {}
        }

        // Outer half: build a namespace per case and re-enter this same test.
        let exe = std::env::current_exe().expect("current_exe");
        for (case, setup) in [
            (
                "deleted-midway",
                "mount -t tmpfs tmpfs /dev && : > /dev/null",
            ),
            ("never-existed", "mount -t tmpfs tmpfs /dev"),
        ] {
            let script = format!(
                "{setup} && exec {} --exact tests::null_stdio_survives_a_deleted_dev_null --ignored --nocapture",
                exe.display()
            );
            let out = std::process::Command::new("unshare")
                .args(["-Urm", "sh", "-c", &script])
                .env(PROBE_VAR, case)
                .output()
                .expect("unshare must be available on Linux CI");
            assert!(
                out.status.success(),
                "case {case} failed:\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
    }

    /// Every spawn takes its own dup, so the helper must survive repeated use
    /// and give the child an immediate EOF (as `/dev/null` does).
    #[cfg(unix)]
    #[test]
    fn null_stdio_reads_eof_every_time() {
        for i in 0..3 {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", "cat"])
                .stdin(null_stdio())
                .stdout(std::process::Stdio::null())
                .status()
                .expect("spawn");
            assert!(status.success(), "spawn {i} must succeed and see EOF");
        }
    }

    /// [`ProcessGroupId`] rejects degenerate pids (0, 1, own group) at
    /// construction so `killpg` can never broadcast outside a child's group.
    #[cfg(unix)]
    #[test]
    fn process_group_id_rejects_degenerate_and_own_group() {
        assert!(
            ProcessGroupId::new(0).is_err(),
            "pid 0 = caller's own group — must be refused"
        );
        assert!(
            ProcessGroupId::new(1).is_err(),
            "pid 1 = init — must be refused"
        );
        let own = nix::unistd::getpgrp().as_raw() as u32;
        assert!(
            ProcessGroupId::new(own).is_err(),
            "the caller's own process group ({own}) must be refused"
        );
        // Pid above i32::MAX wraps on cast — must be refused.
        assert!(
            ProcessGroupId::new(u32::MAX).is_err(),
            "pid > i32::MAX must be refused (wrapping cast in killpg)"
        );
        // A pid that is clearly foreign succeeds.
        let foreign = if own == 2 { 3 } else { 2 };
        let id = ProcessGroupId::new(foreign).expect("foreign pgid should be accepted");
        assert_eq!(id.get(), foreign);
    }
}
