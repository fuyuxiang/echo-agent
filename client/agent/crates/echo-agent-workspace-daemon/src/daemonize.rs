//! Self-daemonization and single-instance locking for the workspace-server.
//!
//! The server is launched fire-and-forget by the sandbox orchestrator, which
//! only ever holds a handle to the originally-spawned PID / process group.
//! After a double-fork + `setsid()` the surviving daemon lives in a new
//! session and process group, so a later process-group kill on the original
//! pgid cannot reach it.
//!
//! The double-fork MUST run before the tokio runtime — or `tracing_subscriber`
//! / the rustls provider — start any threads: forking a multi-threaded process
//! leaves every lock held by a non-forking thread permanently locked in the
//! child, which can deadlock it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use std::{process, thread};
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

use fs2::FileExt;
use prometheus::{IntCounterVec, register_int_counter_vec};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

/// True if `e` reports that an advisory `flock` is held by another process.
/// Unix surfaces this as `WouldBlock`; Windows as `ERROR_LOCK_VIOLATION` (OS
/// error 33), matched via [`fs2::lock_contended_error`].
///
/// A private copy of `echo_agent_workspace::util::is_lock_contended`, which stays
/// in that crate for its other callers. This crate does not depend on it.
fn is_lock_contended(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || (e.raw_os_error().is_some()
            && e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

/// stdout + stderr redirect target when no `--log-file` is given.
#[cfg(unix)]
pub const DEFAULT_LOG_PATH: &str = "/tmp/workspace-server.log";
#[cfg(windows)]
pub const DEFAULT_LOG_PATH: &str = "C:\\Windows\\Temp\\workspace-server.log";

/// Single-instance lock file used when no `--pid-file` is given.
#[cfg(unix)]
pub const DEFAULT_PIDFILE_PATH: &str = "/tmp/workspace-server.pid";
#[cfg(windows)]
pub const DEFAULT_PIDFILE_PATH: &str = "C:\\Windows\\Temp\\workspace-server.pid";

/// How long a takeover waits for the gracefully-terminated predecessor to
/// release the pidfile lock before escalating to a forceful kill.
///
/// Intentionally far below the server's own SIGTERM drain budget
/// (`ECHO_AGENT_WORKSPACE_TERMINATION_GRACE_MS`, default 45s): a takeover only
/// happens when the orchestrator has already declared the incumbent stale,
/// so a bounded ready time for the replacement outranks completing the
/// predecessor's drain.
pub const TAKEOVER_GRACE: Duration = Duration::from_secs(2);

/// How long a takeover waits for the lock after the forceful kill (process
/// death releases the flock) before declining.
const TAKEOVER_KILL_GRACE: Duration = Duration::from_secs(1);

/// Poll interval while waiting for the predecessor to release the lock.
const TAKEOVER_POLL: Duration = Duration::from_millis(50);

/// Invocation fragment identifying a pidfile holder as a workspace-server.
const WORKSPACE_SERVER_NAME_FRAGMENT: &str = "workspace-server";

/// `oom_score_adj` for the workspace-server when `--oom-protect` is set. Not -1000: reserved for
/// init/privileged agents; still killable last-resort.
pub const WORKSPACE_SERVER_OOM_SCORE_ADJ: i32 = -900;

/// `oom_score_adj` for the supervised preview-proxy (between user work at 0 and workspace/files
/// daemons at -900).
pub const PREVIEW_PROXY_OOM_SCORE_ADJ: i32 = -500;

/// `workspace_oom_protect_applied{outcome}`: result of the in-guest self-lowering write when
/// `--oom-protect` is set.
static OOM_PROTECT_APPLIED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "workspace_oom_protect_applied",
        "workspace-server in-guest oom_score_adj self-lowering attempts",
        &["outcome"]
    )
    .unwrap()
});

fn record_oom_protect(outcome: &'static str) {
    OOM_PROTECT_APPLIED.with_label_values(&[outcome]).inc();
}

/// Write `/proc/self/oom_score_adj`. Linux only; checked so a capability
/// regression surfaces as an error rather than silent no-op.
#[cfg(target_os = "linux")]
pub fn set_oom_score_adj(adj: i32) -> io::Result<()> {
    fs::write("/proc/self/oom_score_adj", format!("{adj}\n"))
}

#[cfg(not(target_os = "linux"))]
pub fn set_oom_score_adj(_adj: i32) -> io::Result<()> {
    Ok(())
}

/// Serializes process-global `/proc/self/oom_score_adj` mutation across crate tests.
#[cfg(all(test, target_os = "linux"))]
pub(crate) static TEST_OOM_SCORE_ADJ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Lower the workspace-server's `oom_score_adj` to [`WORKSPACE_SERVER_OOM_SCORE_ADJ`] and record
/// `workspace_oom_protect_applied{outcome}`.
///
/// Call after daemonize (double-fork) and before the runtime starts when
/// `--oom-protect` is set. Complements always-on `protect_from_oom_kill`: the
/// unshare parent may already have lowered the score *before* `--map-root-user`
/// (nested userns lacks init-ns `CAP_SYS_RESOURCE`); an already-applied target
/// counts as success so the metric is truthful. On a true failure logs to
/// stderr and returns `false`; callers still force the child-reset env so a
/// partial lower cannot shield user work.
pub fn apply_workspace_oom_protect() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(cur) = fs::read_to_string("/proc/self/oom_score_adj")
            && cur.trim().parse::<i32>().ok() == Some(WORKSPACE_SERVER_OOM_SCORE_ADJ)
        {
            record_oom_protect("applied");
            return true;
        }
    }
    match set_oom_score_adj(WORKSPACE_SERVER_OOM_SCORE_ADJ) {
        Ok(()) => {
            record_oom_protect("applied");
            true
        }
        Err(e) => {
            // Score may already be at the target (wrapper lowered it; nested
            // userns cannot re-write). Treat that as applied.
            #[cfg(target_os = "linux")]
            if let Ok(cur) = fs::read_to_string("/proc/self/oom_score_adj")
                && cur.trim().parse::<i32>().ok() == Some(WORKSPACE_SERVER_OOM_SCORE_ADJ)
            {
                record_oom_protect("applied");
                return true;
            }
            // stderr is already the log channel when daemonized.
            eprintln!(
                "failed to lower oom_score_adj to {}: {e}",
                WORKSPACE_SERVER_OOM_SCORE_ADJ
            );
            record_oom_protect("failed");
            false
        }
    }
}

/// Double-fork + `setsid()` into a new session, `chdir("/")`, and redirect
/// stdio (stdin ← `/dev/null`, stdout+stderr appended to `log_path`).
///
/// Must be called before any runtime/tracing/TLS threads start (see module docs).
#[cfg(unix)]
pub fn daemonize(log_path: &Path) -> io::Result<()> {
    // First fork: the launcher-tracked parent exits, orphaning the child.
    fork_and_exit_parent()?;

    // New session/process group, detaching the controlling terminal. Must
    // follow a fork — a process-group leader cannot call setsid().
    // SAFETY: `setsid()` takes no pointers; it only changes session membership.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }

    // Second fork: a non-session-leader can never reacquire a controlling tty.
    fork_and_exit_parent()?;

    // Detach from the launch directory (callers capture cwd beforehand).
    // SAFETY: `c"/"` is a 'static, NUL-terminated string valid for the call.
    if unsafe { libc::chdir(c"/".as_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }

    redirect_stdio(log_path)
}

/// Windows daemonization: no fork/setsid (the launcher already backgrounds the
/// server) — only redirect stdout+stderr to the log file. Must run before any
/// stdout/stderr use (Rust caches the std handles on first access). The
/// single-instance lock is taken separately via [`PidFile`].
#[cfg(windows)]
pub fn daemonize(log_path: &Path) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let log = daemon_file_options()
        .create(true)
        .append(true)
        .open(log_path)?;

    let handle = HANDLE(log.as_raw_handle());
    // SAFETY: `handle` is a live file handle owned by `log`; SetStdHandle only
    // records it as the process stdout/stderr. `forget(log)` keeps it open for
    // the process lifetime (the std streams reference it now).
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, handle).map_err(io::Error::other)?;
        SetStdHandle(STD_ERROR_HANDLE, handle).map_err(io::Error::other)?;
    }
    std::mem::forget(log);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn daemonize(_log_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "daemonize is only supported on Unix and Windows",
    ))
}

/// `fork()`; the parent exits 0, the child returns `Ok(())` to continue.
#[cfg(unix)]
fn fork_and_exit_parent() -> io::Result<()> {
    // SAFETY: only called pre-runtime while single-threaded, so the fork
    // cannot strand another thread's lock in the child.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(()),
        _ => process::exit(0),
    }
}

/// `OpenOptions` for a daemon-owned file (log or pidfile). On Unix it adds
/// `O_NOFOLLOW` + mode `0600` as symlink/permission defense-in-depth; the
/// per-tenant sandbox namespace is the primary control. Shared with the
/// preview-proxy log (`preview_supervisor`) so both daemon-owned files get the
/// same posture.
#[cfg(unix)]
pub(crate) fn daemon_file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = OpenOptions::new();
    opts.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    opts
}

#[cfg(not(unix))]
pub(crate) fn daemon_file_options() -> OpenOptions {
    OpenOptions::new()
}

/// Open `/dev/null` (read) for stdin and `log_path` (created, append) for
/// stdout + stderr.
#[cfg(unix)]
fn open_stdio_targets(log_path: &Path) -> io::Result<(File, File)> {
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let stdin_src = OpenOptions::new().read(true).open("/dev/null")?;
    let log = daemon_file_options()
        .create(true)
        .append(true)
        .open(log_path)?;
    Ok((stdin_src, log))
}

/// `dup2(source, target)`, mapping the `-1` sentinel to an `io::Error`.
#[cfg(unix)]
fn redirect_fd(target: RawFd, source: &File) -> io::Result<()> {
    // SAFETY: `source` is an open File and `target` a standard descriptor —
    // both valid for `dup2`.
    if unsafe { libc::dup2(source.as_raw_fd(), target) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn redirect_stdio(log_path: &Path) -> io::Result<()> {
    let (stdin_src, log) = open_stdio_targets(log_path)?;
    redirect_fd(libc::STDIN_FILENO, &stdin_src)?;
    redirect_fd(libc::STDOUT_FILENO, &log)?;
    redirect_fd(libc::STDERR_FILENO, &log)?;
    // `stdin_src` / `log` close here; fds 0/1/2 keep their dup'd copies.
    Ok(())
}

/// Single-instance lock backed by an advisory `flock` on a pidfile, held for
/// the daemon's lifetime. Dropping it closes the file, releasing the lock; the
/// pidfile itself is left on disk for diagnostics.
#[derive(Debug)]
pub struct PidFile {
    _file: File,
}

impl PidFile {
    /// Take the exclusive lock and record the current PID.
    ///
    /// - `Ok(Some(_))` — lock acquired; hold the returned guard.
    /// - `Ok(None)` — another live process holds the lock (caller should
    ///   no-op and exit cleanly).
    /// - `Err(_)` — an I/O error opening or locking the file.
    pub fn acquire(path: &Path) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut file = daemon_file_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if is_lock_contended(&e) => return Ok(None),
            Err(e) => return Err(e),
        }

        // PID contents are advisory diagnostics; the flock provides exclusion.
        // `set_len(0)` clears any stale (possibly longer) value first.
        file.set_len(0)?;
        file.write_all(process::id().to_string().as_bytes())?;
        file.flush()?;

        Ok(Some(Self { _file: file }))
    }

    /// Acquire the lock, taking over from a live predecessor workspace-server
    /// if one holds it: graceful termination (its normal drain runs), `grace`
    /// to release the lock, then a forceful kill (process death releases the
    /// flock). The lock is never bypassed — a guard is returned only with the
    /// flock held.
    ///
    /// `Ok(None)` means the caller should exit quietly: the holder is not an
    /// identifiable workspace-server, or the lock could not be won after the
    /// escalation (e.g. a concurrent newer spawn took it).
    pub fn acquire_or_take_over(path: &Path, grace: Duration) -> io::Result<Option<Self>> {
        Self::acquire_or_take_over_matching(path, grace, WORKSPACE_SERVER_NAME_FRAGMENT)
    }

    /// [`Self::acquire_or_take_over`] with an injectable name fragment so
    /// tests can match their own predecessor processes.
    fn acquire_or_take_over_matching(
        path: &Path,
        grace: Duration,
        name_fragment: &str,
    ) -> io::Result<Option<Self>> {
        if let Some(guard) = Self::acquire(path)? {
            return Ok(Some(guard));
        }

        let Some(pid) = read_pidfile_pid(path) else {
            return Ok(None);
        };
        if pid == process::id() {
            return Ok(None);
        }
        let Some(predecessor) = PredecessorTarget::open(pid, name_fragment) else {
            return Ok(None);
        };

        // tracing is not initialized this early; in daemonized mode stderr is
        // already redirected to the log file, so eprintln! is the log channel.
        eprintln!("taking over from predecessor workspace-server (pid {pid})");
        if let Err(e) = predecessor.signal(false) {
            eprintln!("failed to signal predecessor (pid {pid}): {e}");
        }
        if let Some(guard) = Self::poll_acquire(path, grace)? {
            return Ok(Some(guard));
        }

        eprintln!("predecessor (pid {pid}) did not release the pidfile lock in time; killing it");
        if let Err(e) = predecessor.signal(true) {
            eprintln!("failed to kill predecessor (pid {pid}): {e}");
        }
        if let Some(guard) = Self::poll_acquire(path, TAKEOVER_KILL_GRACE)? {
            return Ok(Some(guard));
        }

        // The holder we signaled is dead yet the lock is still owned — a
        // concurrent newer spawn won it. Decline rather than double-run.
        eprintln!("pidfile lock is still held after killing pid {pid}; exiting");
        Ok(None)
    }

    /// Retry [`Self::acquire`] until it succeeds or `budget` elapses.
    fn poll_acquire(path: &Path, budget: Duration) -> io::Result<Option<Self>> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(guard) = Self::acquire(path)? {
                return Ok(Some(guard));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(TAKEOVER_POLL);
        }
    }
}

/// Advisory pid recorded in the pidfile by its holder; `None` if unreadable
/// or not a positive integer.
fn read_pidfile_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid > 0)
}

/// True if the basename of `name` (path separators `/` and `\` both count)
/// contains `fragment`. Matching the basename rather than the whole path
/// keeps a directory component like `/var/lib/workspace-server-data/foo` from
/// satisfying the kill gate.
#[cfg(any(test, target_os = "linux", windows))]
fn basename_contains(name: &str, fragment: &str) -> bool {
    name.rsplit(['/', '\\']).next().is_some_and(|base| {
        base.to_ascii_lowercase()
            .contains(&fragment.to_ascii_lowercase())
    })
}

/// True if `pid`'s argv0 basename (from `/proc/<pid>/cmdline`) matches
/// `fragment`.
#[cfg(target_os = "linux")]
fn process_name_matches(pid: u32, fragment: &str) -> bool {
    match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => cmdline
            .split(|&b| b == 0)
            .next()
            .is_some_and(|argv0| basename_contains(&String::from_utf8_lossy(argv0), fragment)),
        Err(_) => false,
    }
}

/// A pinned, verified handle to the predecessor process: `pidfd_open(2)` on
/// Linux, an `OpenProcess` handle on Windows.
///
/// Pinning happens **before** verification and every signal is delivered
/// through the pin, closing the check-then-kill pid-reuse race by
/// construction: a recycled pid is unreachable — at worst a signal lands on
/// the already-dead pinned instance and is a no-op.
#[cfg(target_os = "linux")]
struct PredecessorTarget {
    pid: u32,
    /// `None` = pidfd unsupported on this kernel; plain-`kill` fallback mode
    /// (retains only the historical residual race).
    pidfd: Option<OwnedFd>,
}

#[cfg(target_os = "linux")]
impl PredecessorTarget {
    /// Pin `pid` and verify its executable basename matches `fragment`.
    /// `None` if the process is gone, inaccessible, or not a match.
    fn open(pid: u32, fragment: &str) -> Option<Self> {
        // SAFETY: pidfd_open takes value arguments only; the returned fd is
        // fresh and exclusively owned here.
        let ret = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
        let pidfd = if ret >= 0 {
            // SAFETY: `ret` is a freshly returned, unowned fd.
            Some(unsafe { OwnedFd::from_raw_fd(ret as RawFd) })
        } else {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ESRCH) {
                return None;
            }
            // ENOSYS or seccomp-filtered: degrade to unpinned kill().
            None
        };

        // Verify after pinning: a pid recycled before the pin fails the name
        // match; recycled after, the pin targets the dead predecessor.
        if !process_name_matches(pid, fragment) {
            return None;
        }
        Some(Self { pid, pidfd })
    }

    /// Deliver graceful (SIGTERM) or forceful (SIGKILL) termination to the
    /// pinned instance. Already-dead is `Ok`.
    fn signal(&self, forceful: bool) -> io::Result<()> {
        let signal = if forceful {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        let ret = match &self.pidfd {
            // SAFETY: the pidfd is owned and open; the siginfo pointer is
            // documented-null (kernel builds a default), flags are zero.
            Some(fd) => unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0u32,
                )
            },
            // SAFETY: kill() takes no pointers.
            None => unsafe { libc::kill(self.pid as libc::pid_t, signal) }.into(),
        };
        if ret == 0 {
            return Ok(());
        }
        match io::Error::last_os_error() {
            e if e.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            e => Err(e),
        }
    }
}

#[cfg(windows)]
struct PredecessorTarget {
    handle: HANDLE,
}

// SAFETY: the HANDLE is an owned kernel object reference; it is not tied to
// the creating thread and is only used behind &self.
#[cfg(windows)]
unsafe impl Send for PredecessorTarget {}

#[cfg(windows)]
impl PredecessorTarget {
    /// Pin `pid` with query + terminate rights and verify the image basename
    /// matches `fragment` on the pinned handle.
    fn open(pid: u32, fragment: &str) -> Option<Self> {
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
            QueryFullProcessImageNameW,
        };
        use windows::core::PWSTR;

        // SAFETY: OpenProcess is FFI with value args; windows-rs returns Err
        // on absence/permission failure.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                false,
                pid,
            )
        }
        .ok()?;
        let target = Self { handle };

        // QueryFullProcessImageNameW writes a NUL-terminated UTF-16 path into
        // the buffer; `size` is updated to the chars written (excluding NUL).
        let mut buf: Vec<u16> = vec![0; 1024];
        let mut size: u32 = buf.len() as u32;
        // SAFETY: the handle is pinned by `target`; buf outlives the call;
        // size is in/out.
        let result = unsafe {
            QueryFullProcessImageNameW(
                target.handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        };
        if result.is_err() {
            return None;
        }
        basename_contains(&String::from_utf16_lossy(&buf[..size as usize]), fragment)
            .then_some(target)
    }

    /// `TerminateProcess` on the pinned handle (Windows has no graceful
    /// signal for a detached process). Already-dead is `Ok`.
    fn signal(&self, _forceful: bool) -> io::Result<()> {
        use windows::Win32::System::Threading::TerminateProcess;

        // SAFETY: the handle is the pinned kernel object owned by self.
        match unsafe { TerminateProcess(self.handle, 0) } {
            Ok(()) => Ok(()),
            // Already exited: terminating a dead (but pinned) process fails
            // with access-style errors; the takeover treats that as done.
            Err(e) => Err(io::Error::other(format!("TerminateProcess: {e}"))),
        }
    }
}

#[cfg(windows)]
impl Drop for PredecessorTarget {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        // SAFETY: the handle is owned by self and closed exactly once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
struct PredecessorTarget;

#[cfg(not(any(target_os = "linux", windows)))]
impl PredecessorTarget {
    /// Unsupported platform: never identify a predecessor (takeover declines
    /// rather than kill blind).
    fn open(_pid: u32, _fragment: &str) -> Option<Self> {
        None
    }

    fn signal(&self, _forceful: bool) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process termination is only supported on Linux and Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    // Used only by the linux-gated predecessor-takeover tests below.
    #[cfg(target_os = "linux")]
    use std::process::{Child, Command, Stdio};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn pidfile_acquire_is_exclusive() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let first = PidFile::acquire(&path).unwrap();
        assert!(first.is_some(), "first acquire should win the lock");

        // A second open of the same path conflicts on the advisory flock,
        // even within the same process (flock is per open file description).
        let second = PidFile::acquire(&path).unwrap();
        assert!(second.is_none(), "contended acquire must report None");

        drop(first);

        // Dropping the guard closes the fd and releases the flock. Retry briefly:
        // under the parallel test runner a concurrent `fork`/`Command::spawn` can
        // transiently duplicate this flock'd fd, holding the lock until the child
        // `execve`s (the fd is `O_CLOEXEC`). That window is microseconds, so a
        // short bounded retry makes the release deterministic without weakening
        // the held-exclusion assertion above.
        let deadline = Instant::now() + Duration::from_secs(2);
        let third = loop {
            match PidFile::acquire(&path).unwrap() {
                Some(guard) => break Some(guard),
                None if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                none => break none,
            }
        };
        assert!(third.is_some(), "acquire should succeed after release");
    }

    #[test]
    fn pidfile_records_current_pid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let guard = PidFile::acquire(&path).unwrap().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim().parse::<u32>().unwrap(), process::id());
        drop(guard);
    }

    #[test]
    fn pidfile_persists_on_disk_after_drop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let guard = PidFile::acquire(&path).unwrap().unwrap();
        assert!(path.exists());
        drop(guard);
        // The file is intentionally left behind for diagnostics; only the
        // lock is released (re-acquirable, covered by the exclusivity test).
        assert!(path.exists(), "pidfile should remain on disk after drop");
    }

    #[test]
    fn pidfile_acquire_creates_parent_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/sub/ws.pid");

        let guard = PidFile::acquire(&path).unwrap();
        assert!(guard.is_some());
        assert!(path.exists());
    }

    #[test]
    fn pidfile_acquire_truncates_stale_longer_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");
        // A leftover value longer than our PID would leave trailing bytes if
        // `set_len(0)` were missing.
        fs::write(&path, "999999999999 stale junk\n").unwrap();

        let guard = PidFile::acquire(&path).unwrap().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents,
            process::id().to_string(),
            "stale content must be fully truncated, no trailing bytes"
        );
        drop(guard);
    }

    #[test]
    fn contended_acquire_does_not_modify_pidfile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let holder = PidFile::acquire(&path).unwrap().unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let contended = PidFile::acquire(&path).unwrap();
        assert!(contended.is_none());

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "contended acquire must not rewrite the file");
        drop(holder);
    }

    #[test]
    fn pidfile_acquire_errors_on_directory() {
        let dir = TempDir::new().unwrap();
        let as_dir = dir.path().join("a_dir");
        fs::create_dir(&as_dir).unwrap();

        // Opening a directory for writing yields EISDIR — a real error that
        // must surface as `Err`, never be swallowed into `Ok(None)`.
        assert!(
            PidFile::acquire(&as_dir).is_err(),
            "acquiring a directory path must error, not report Ok(None)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_stdio_targets_opens_devnull_and_log() {
        use std::io::{Read, Write};

        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("logs/ws.log");

        let (mut stdin_src, mut log) = open_stdio_targets(&log_path).unwrap();
        assert!(log_path.exists(), "log file should be created");

        log.write_all(b"hello").unwrap();
        log.flush().unwrap();
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "hello");

        // The stdin source is /dev/null: reads yield EOF immediately.
        let mut buf = [0u8; 4];
        assert_eq!(stdin_src.read(&mut buf).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn open_stdio_targets_appends_to_existing_log() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("ws.log");
        fs::write(&log_path, "prior\n").unwrap();

        let (_stdin_src, mut log) = open_stdio_targets(&log_path).unwrap();
        log.write_all(b"more\n").unwrap();
        log.flush().unwrap();

        assert_eq!(fs::read_to_string(&log_path).unwrap(), "prior\nmore\n");
    }

    #[cfg(unix)]
    #[test]
    fn open_stdio_targets_errors_when_parent_is_a_file() {
        let dir = TempDir::new().unwrap();
        let parent_file = dir.path().join("not_a_dir");
        fs::write(&parent_file, "x").unwrap();
        // `not_a_dir` is a regular file, so a log path under it is ENOTDIR.
        let log_path = parent_file.join("ws.log");

        assert!(
            open_stdio_targets(&log_path).is_err(),
            "a log path whose parent is a file must error"
        );
    }

    // O_NOFOLLOW makes a symlinked final component fail with ELOOP rather than
    // being followed — deterministic and uid-independent (no chmod, root-safe).
    #[cfg(unix)]
    #[test]
    fn open_stdio_targets_rejects_symlinked_log() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.log");
        fs::write(&target, "").unwrap();
        let link = dir.path().join("link.log");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = open_stdio_targets(&link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
    }

    #[cfg(unix)]
    #[test]
    fn pidfile_acquire_rejects_symlinked_path() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.pid");
        let link = dir.path().join("link.pid");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = PidFile::acquire(&link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        // The truncate-through-symlink primitive is blocked: O_CREAT did not
        // follow the link to create (and truncate) its target.
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn pidfile_created_mode_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let _guard = PidFile::acquire(&path).unwrap().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        // No group/other bits, regardless of umask (0600 & ~umask keeps them 0).
        assert_eq!(
            mode & 0o077,
            0,
            "pidfile must not be group/other-accessible"
        );
    }

    #[cfg(not(any(unix, windows)))]
    #[test]
    fn daemonize_unsupported_off_unix_and_windows() {
        let err = daemonize(Path::new("ignored")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn take_over_uncontended_acquires_normally() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let guard = PidFile::acquire_or_take_over(&path, Duration::from_millis(100)).unwrap();
        assert!(guard.is_some());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            process::id().to_string()
        );
    }

    #[test]
    fn take_over_declines_unreadable_pidfile() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let _holder = PidFile::acquire(&path).unwrap().unwrap();
        fs::write(&path, "not a pid").unwrap();

        let taken =
            PidFile::acquire_or_take_over_matching(&path, Duration::from_millis(100), "sleep")
                .unwrap();
        assert!(taken.is_none(), "an unidentifiable holder must be declined");
    }

    #[test]
    fn take_over_declines_own_pid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        // The in-process holder wrote our own pid; a takeover must not
        // signal ourselves.
        let _holder = PidFile::acquire(&path).unwrap().unwrap();
        let taken =
            PidFile::acquire_or_take_over_matching(&path, Duration::from_millis(100), "").unwrap();
        assert!(taken.is_none());
    }

    /// Spawn a long-sleeping child to stand in for a predecessor process.
    #[cfg(target_os = "linux")]
    #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
    fn spawn_predecessor() -> Child {
        Command::new("sleep")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    /// Wait (bounded) for a child to exit; returns true if it did.
    #[cfg(target_os = "linux")]
    fn wait_for_exit(child: &mut Child, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if child.try_wait().expect("try_wait").is_some() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn take_over_declines_non_matching_holder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let _holder = PidFile::acquire(&path).unwrap().unwrap();
        let mut child = spawn_predecessor();
        fs::write(&path, child.id().to_string()).unwrap();

        let taken = PidFile::acquire_or_take_over_matching(
            &path,
            Duration::from_millis(100),
            "definitely-not-this-process",
        )
        .unwrap();
        assert!(taken.is_none(), "a foreign holder must not be taken over");
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "a foreign holder must not be killed"
        );

        child.kill().expect("cleanup kill");
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn take_over_declines_when_lock_is_never_released() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        // The flock is held in-process for the whole test — after the child
        // named in the pidfile is dead, the lock is still owned by "someone
        // else" (a concurrent-spawn stand-in), so the takeover must decline
        // rather than run without single-instance protection.
        let _holder = PidFile::acquire(&path).unwrap().unwrap();
        let mut child = spawn_predecessor();
        let child_pid = child.id();
        fs::write(&path, child_pid.to_string()).unwrap();

        let taken =
            PidFile::acquire_or_take_over_matching(&path, Duration::from_millis(300), "sleep")
                .unwrap();

        assert!(
            wait_for_exit(&mut child, Duration::from_secs(2)),
            "the predecessor must be terminated"
        );
        assert!(
            taken.is_none(),
            "a takeover that cannot win the lock must decline, never proceed lockless"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            child_pid.to_string(),
            "a declined takeover must not rewrite the pidfile"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn take_over_escalates_to_sigkill_for_stuck_predecessor() {
        use std::os::unix::process::ExitStatusExt as _;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        // A predecessor that ignores the graceful signal: only the SIGKILL
        // escalation can end it. It touches a marker once the trap is
        // installed so the test cannot signal it during bash startup.
        let trap_ready = dir.path().join("trap-ready");
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "trap '' TERM; touch {}; while true; do sleep 1; done",
                trap_ready.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn stubborn child");
        let trap_deadline = Instant::now() + Duration::from_secs(5);
        while !trap_ready.exists() {
            assert!(Instant::now() < trap_deadline, "child never set its trap");
            thread::sleep(Duration::from_millis(10));
        }

        // Stand in for the stuck predecessor's flock: released only after the
        // graceful grace has expired, inside the post-kill window.
        let holder = PidFile::acquire(&path).unwrap().unwrap();
        fs::write(&path, child.id().to_string()).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            drop(holder);
        });

        let taken =
            PidFile::acquire_or_take_over_matching(&path, Duration::from_millis(300), "bash")
                .unwrap();
        release.join().expect("release thread");

        let status = child.wait().expect("child wait");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "a SIGTERM-immune predecessor must be ended by the SIGKILL escalation"
        );
        assert!(taken.is_some(), "the lock freed within the kill window");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            process::id().to_string()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn take_over_acquires_cleanly_when_predecessor_releases() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        let holder = PidFile::acquire(&path).unwrap().unwrap();
        let mut child = spawn_predecessor();
        fs::write(&path, child.id().to_string()).unwrap();

        // Release the lock shortly after the takeover starts waiting,
        // simulating the predecessor finishing its drain within grace.
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(holder);
        });

        let taken =
            PidFile::acquire_or_take_over_matching(&path, Duration::from_secs(5), "sleep").unwrap();
        release.join().expect("release thread");

        assert!(
            wait_for_exit(&mut child, Duration::from_secs(2)),
            "the predecessor must be terminated"
        );
        assert!(taken.is_some());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            process::id().to_string()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_name_matches_own_argv0() {
        let pid = process::id();
        // Derive the fragment from this process's real argv0 basename rather
        // than hardcoding a name: different test runners name the binary
        // differently (e.g. Cargo uses `echo_agent_workspace_daemon-<hash>`), so a
        // hardcoded fragment matches under one runner but not another.
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).expect("read own cmdline");
        let argv0 = cmdline.split(|&b| b == 0).next().expect("argv0 present");
        let basename = String::from_utf8_lossy(argv0)
            .rsplit(['/', '\\'])
            .next()
            .expect("basename")
            .to_owned();
        assert!(!basename.is_empty(), "argv0 basename must not be empty");
        assert!(process_name_matches(pid, &basename));
        assert!(!process_name_matches(pid, "definitely-not-this-process"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn predecessor_target_pins_verifies_and_signals() {
        let mut child = spawn_predecessor();

        assert!(
            PredecessorTarget::open(child.id(), "not-a-match").is_none(),
            "a non-matching name must not produce a target"
        );

        // /proc/<pid>/cmdline can lag briefly after spawn under remote CI
        // executors; derive the fragment from the live cmdline (handles
        // busybox-as-sleep) and retry pin open instead of a one-shot expect.
        let fragment = {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(cmdline) = fs::read(format!("/proc/{}/cmdline", child.id())) {
                    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or_default();
                    let basename = String::from_utf8_lossy(argv0)
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    if !basename.is_empty() {
                        break basename;
                    }
                }
                if Instant::now() >= deadline {
                    panic!("child cmdline never became readable");
                }
                thread::sleep(Duration::from_millis(10));
            }
        };

        let target = {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(t) = PredecessorTarget::open(child.id(), &fragment) {
                    break t;
                }
                if Instant::now() >= deadline {
                    panic!("pin child (fragment={fragment:?})");
                }
                thread::sleep(Duration::from_millis(10));
            }
        };
        target.signal(false).expect("graceful signal");
        assert!(
            wait_for_exit(&mut child, Duration::from_secs(2)),
            "the pinned child must receive the signal"
        );

        // Signalling the dead pinned instance is a no-op, never a stray kill.
        target
            .signal(true)
            .expect("signal to dead pinned instance is Ok");
    }

    #[test]
    fn basename_contains_ignores_directory_components() {
        assert!(basename_contains(
            "/usr/local/bin/echo-agent-workspace-server",
            "workspace-server"
        ));
        assert!(basename_contains(
            "C:\\Program Files\\ECHO_AGENT-Workspace-Server.exe",
            "workspace-server"
        ));
        assert!(
            !basename_contains(
                "/var/lib/workspace-server-data/unrelated",
                "workspace-server"
            ),
            "a matching directory component must not satisfy the kill gate"
        );
    }

    #[test]
    fn read_pidfile_pid_parses_and_rejects() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ws.pid");

        fs::write(&path, "1234\n").unwrap();
        assert_eq!(read_pidfile_pid(&path), Some(1234));

        fs::write(&path, "0").unwrap();
        assert_eq!(read_pidfile_pid(&path), None, "pid 0 is not a process");

        fs::write(&path, "garbage").unwrap();
        assert_eq!(read_pidfile_pid(&path), None);

        assert_eq!(read_pidfile_pid(&dir.path().join("missing")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn set_oom_score_adj_lowers_when_permitted() {
        let _guard = super::TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let own = fs::read_to_string("/proc/self/oom_score_adj").expect("read own score");
        let restore = own.trim().to_owned();
        // Lowering below 0 needs CAP_SYS_RESOURCE; skip when unavailable (CI host).
        if set_oom_score_adj(WORKSPACE_SERVER_OOM_SCORE_ADJ).is_err() {
            return;
        }
        let after = fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        assert_eq!(after.trim(), WORKSPACE_SERVER_OOM_SCORE_ADJ.to_string());
        let _ = fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apply_workspace_oom_protect_records_outcome() {
        let _guard = super::TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let before_applied = OOM_PROTECT_APPLIED.with_label_values(&["applied"]).get();
        let before_failed = OOM_PROTECT_APPLIED.with_label_values(&["failed"]).get();
        let ok = apply_workspace_oom_protect();
        if ok {
            assert!(OOM_PROTECT_APPLIED.with_label_values(&["applied"]).get() > before_applied);
            // Restore default so later tests are not OOM-immune.
            let _ = set_oom_score_adj(0);
        } else {
            assert!(OOM_PROTECT_APPLIED.with_label_values(&["failed"]).get() > before_failed);
        }
    }

    /// Pre-unshare lower leaves the score already at the target; the binary's
    /// re-check must count as `applied`, not `failed`.
    #[cfg(target_os = "linux")]
    #[test]
    fn apply_workspace_oom_protect_already_at_target_counts_as_applied() {
        let _guard = super::TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let own = fs::read_to_string("/proc/self/oom_score_adj").expect("read own score");
        let restore = own.trim().to_owned();
        // Precondition needs CAP_SYS_RESOURCE to plant -900.
        if set_oom_score_adj(WORKSPACE_SERVER_OOM_SCORE_ADJ).is_err() {
            return;
        }
        let before_applied = OOM_PROTECT_APPLIED.with_label_values(&["applied"]).get();
        let before_failed = OOM_PROTECT_APPLIED.with_label_values(&["failed"]).get();

        assert!(
            apply_workspace_oom_protect(),
            "already-at-target must report success"
        );
        assert!(
            OOM_PROTECT_APPLIED.with_label_values(&["applied"]).get() > before_applied,
            "already-at-target must increment applied"
        );
        assert_eq!(
            OOM_PROTECT_APPLIED.with_label_values(&["failed"]).get(),
            before_failed,
            "already-at-target must not increment failed"
        );

        let _ = fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
    }

    #[test]
    fn oom_score_constants_preserve_ordering() {
        // user work (0) > proxy (-500) > workspace/files (-900); never -1000.
        const {
            assert!(PREVIEW_PROXY_OOM_SCORE_ADJ < 0);
            assert!(WORKSPACE_SERVER_OOM_SCORE_ADJ < PREVIEW_PROXY_OOM_SCORE_ADJ);
            assert!(WORKSPACE_SERVER_OOM_SCORE_ADJ > -1000);
        }
    }
}
