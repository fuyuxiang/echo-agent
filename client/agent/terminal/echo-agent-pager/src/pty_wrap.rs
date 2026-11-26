//! Local PTY wrapper: the engine behind `echoagent wrap` (see [`crate::wrap_cmd`]).
//!
//! Spawns a command inside a local pseudo-terminal and pipes its output
//! through `crate::wrap_filter::Osc52Filter`, which intercepts OSC 52
//! clipboard sequences (making "copy" work for programs that cannot reach the
//! user's clipboard — containers, SSH — even under terminals without OSC 52
//! support), answers the private host clipboard image request OSC (see
//! [`crate::wrap_clipboard_image`]), and reports DEC private mode changes to
//! `crate::wrap_restore::ModeTracker`.
//!
//! This module owns the process plumbing: PTY setup, the writer/stdin/resize
//! threads, and — via the tracker — the exit paths that restore the outer
//! terminal (drop guard, termination-signal thread) when the wrapped command
//! dies with modes still latched.

use anyhow::Result;
use std::io::Write;
use std::sync::Arc;

use crate::theme::system_appearance::SystemAppearance;
use crate::wrap_filter::Osc52Filter;
use crate::wrap_restore::ModeTracker;

/// OSC 52 sink markers plus optional local appearance (`LC_*` survives SSH).
fn apply_wrap_child_env(
    cmd: &mut portable_pty::CommandBuilder,
    appearance: Option<SystemAppearance>,
) {
    cmd.env("ECHO_AGENT_OSC52_SINK", "1");
    cmd.env("LC_ECHO_AGENT_OSC52_SINK", "1");
    if let Some(appearance) = appearance {
        let value = appearance.as_env_value();
        cmd.env("ECHO_AGENT_APPEARANCE", value);
        cmd.env("LC_ECHO_AGENT_APPEARANCE", value);
    }
}

/// Run an arbitrary command inside a local PTY with OSC 52 output filtering.
///
/// This is the engine behind `echoagent wrap`: it spawns
/// `program` (with `args`) attached to a local pseudo-terminal, forwards the
/// outer terminal's size changes to it, and filters its output through
/// `Osc52Filter`, which intercepts OSC 52 clipboard sequences and writes
/// their payload to the local system clipboard. All other output passes
/// through unchanged.
///
/// Returns the child exit code on success.
pub(crate) fn run_wrapped_command(program: &str, args: &[String]) -> Result<i32> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read;

    // Get current terminal size.
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // Open PTY pair.
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Build the command.
    let mut cmd = CommandBuilder::new(program);
    args.iter().for_each(|arg| cmd.arg(arg));

    apply_wrap_child_env(&mut cmd, crate::theme::system_appearance::detect_desktop());

    // Not session-scoped: this is the wrapped process itself.
    #[allow(clippy::disallowed_methods)]
    let mut child = pair.slave.spawn_command(cmd)?;
    // Drop the slave so we get EOF when child exits.
    drop(pair.slave);

    // Obtain reader from the master PTY. All master *writes* (keystrokes +
    // host-image inject frames) go through one dedicated writer thread via a
    // channel. Cross-thread use of portable-pty's `Write` impl observed EIO
    // (errno 5) on macOS even for small inject payloads; confining `write_all`
    // to a single owner thread avoids that. Handles are intentionally
    // detached — `echoagent wrap` is short-lived and exits with the child.
    let mut pty_reader = pair.master.try_clone_reader()?;

    // NOTE: we intentionally do NOT block SIGWINCH here. The resize handler
    // (`sigwinch_loop`) installs a real signal handler via `signal-hook`,
    // which must be free to run when the signal is delivered. Blocking it and
    // waiting via `sigwait` looks correct but silently fails on macOS (see
    // `sigwinch_loop`).

    // Tracks the DEC private modes / kitty pushes flowing through the output
    // filter so every exit path can reset exactly what the child left latched
    // (a connection drop kills the child before its reset bytes arrive).
    let tracker = Arc::new(ModeTracker::new());

    // Switch to raw mode so keystrokes pass through unchanged.
    crossterm::terminal::enable_raw_mode()?;
    let _restore_guard = TerminalRestoreGuard {
        tracker: Arc::clone(&tracker),
    };

    // Terminating signals (external kill, terminal-close HUP) bypass Drop, so
    // handle them explicitly: forward to the child, restore, exit 128+N.
    // Handlers are installed here on the main thread so no signal can slip
    // through before the loop thread gets scheduled.
    #[cfg(unix)]
    let child_reaped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        let child_pid = child.process_id();
        let tracker = Arc::clone(&tracker);
        let child_reaped = Arc::clone(&child_reaped);
        match signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM]) {
            Ok(signals) => {
                std::thread::spawn(move || {
                    terminate_signal_loop(signals, tracker, child_pid, child_reaped)
                });
            }
            Err(e) => tracing::debug!("failed to install wrap termination handler: {e}"),
        }
    }

    let (write_tx, write_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    {
        let mut writer = pair.master.take_writer()?;
        std::thread::spawn(move || {
            while let Ok(bytes) = write_rx.recv() {
                // Single-thread write_all: no libc chunking. EIO here almost
                // always means the slave has closed (child exited); stop.
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    // Local stdin -> writer thread.
    let stdin_tx = write_tx.clone();
    let _stdin_handle = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // SIGWINCH handling thread: resize the PTY when the outer terminal changes size.
    // We move `pair.master` here since we've already cloned the reader and
    // taken the writer above.
    //
    // Unix only: there is no SIGWINCH on Windows. On Windows `pair.master` is
    // instead kept alive inside `pair` until this function returns (after
    // `child.wait`), so the ConPTY stays open for the read loop — the OSC 52
    // clipboard bridge works identically, only live resize is unavailable.
    #[cfg(unix)]
    {
        let master = pair.master;
        std::thread::spawn(move || {
            sigwinch_loop(master);
        });
    }

    // Output forwarding with OSC 52 filtering: PTY reader -> filter -> stdout.
    // Host-image requests: spawn a short-lived worker for clipboard I/O (can be
    // hundreds of ms / osascript) then enqueue the bracketed-paste frame on the
    // writer thread (+ NL so ICANON slaves deliver without another key). Paste
    // mashing can spawn multiple workers; fine for a short-lived wrap process.
    {
        let mut stdout = std::io::stdout().lock();
        let mut filter = Osc52Filter::new()
            .with_wrap_image_handler(move || {
                let tx = write_tx.clone();
                std::thread::spawn(move || {
                    let mut bytes = crate::wrap_filter::host_clipboard_image_frame();
                    bytes.push(b'\n');
                    let _ = tx.send(bytes);
                });
            })
            .with_mode_tracker(Arc::clone(&tracker));
        let mut buf = [0u8; 8192];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let filtered = filter.feed(&buf[..n]);
                    if !filtered.is_empty() {
                        if stdout.write_all(&filtered).is_err() {
                            break;
                        }
                        let _ = stdout.flush();
                    }
                }
            }
        }
    }

    // Wait for child and extract exit code.
    let status = child.wait()?;
    // Reaped: the pid is recyclable from here on, so the signal thread must
    // no longer forward to it.
    #[cfg(unix)]
    child_reaped.store(true, std::sync::atomic::Ordering::SeqCst);
    let code = status.exit_code() as i32;

    Ok(code)
}

/// Wait for `SIGWINCH` and resize the PTY master to match the outer terminal.
///
/// Uses `signal-hook` to install a real signal handler (self-pipe based).
///
/// A previous implementation blocked SIGWINCH and dequeued it with `nix`'s
/// `sigwait`. That pattern is POSIX-correct but **silently fails on macOS**:
/// SIGWINCH's default disposition is "ignore", and macOS discards a blocked
/// default-ignore signal rather than leaving it pending for `sigwait`. The
/// handler never woke, so the inner PTY was never resized and the remote TUI
/// kept rendering at the original size — producing overlapping/stale frames
/// after the user resized their terminal. Installing an actual handler
/// overrides the default-ignore disposition, which is what makes it work.
#[cfg(unix)]
fn sigwinch_loop(master: Box<dyn portable_pty::MasterPty + Send>) {
    use portable_pty::PtySize;

    let mut signals = match signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH]) {
        Ok(signals) => signals,
        Err(e) => {
            tracing::debug!("failed to install SIGWINCH handler: {e}");
            return;
        }
    };

    for _ in signals.forever() {
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }
}

/// Guard that restores terminal state when dropped (including on panic).
///
/// Covers child EOF (the connection-drop path), a `wait()` error, and panics:
/// emits resets for whatever the child left latched, then leaves raw mode.
/// Shares the tracker's one-shot gate with the termination-signal thread so
/// the restore never runs twice.
struct TerminalRestoreGuard {
    tracker: Arc<ModeTracker>,
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        restore_terminal(&self.tracker);
    }
}

/// Idempotently restore the outer terminal: emit resets for the latched
/// modes (only while stdout is still a TTY), then leave raw mode.
///
/// Exactly one caller wins the tracker's claim and emits; every other exit
/// path blocks (bounded) until the winner finishes, because both callers sit
/// directly in front of a `process::exit` (`wrap_cmd::run` after the drop
/// guard, `terminate_signal_loop` after this call) and an exit racing the
/// winner would kill the process mid-restore — raw mode kept, resets partial.
fn restore_terminal(tracker: &ModeTracker) {
    use std::io::IsTerminal;

    if !tracker.begin_restore() {
        wait_restore_done(tracker, std::time::Duration::from_millis(100));
        return;
    }
    let bytes = crate::wrap_restore::restore_bytes(tracker.snapshot());
    if !bytes.is_empty() && std::io::stdout().is_terminal() {
        write_stdout_unlocked(&bytes);
    }
    let _ = crossterm::terminal::disable_raw_mode();
    tracker.finish_restore();
}

/// Bounded wait for a claimed restore to complete; `true` when it did.
///
/// The bound is load-bearing: the claim winner's `write(2)` can block
/// indefinitely on a flow-controlled TTY, and an unbounded wait here would
/// reintroduce the never-exits failure mode the unlocked write exists to
/// avoid. On timeout the caller proceeds to exit with a possibly-partial
/// restore — no worse than losing the race outright.
fn wait_restore_done(tracker: &ModeTracker, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if tracker.restore_done() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Best-effort write of restore bytes to stdout, bypassing Rust's stdout lock.
///
/// The signal path can fire while the read loop holds the locked stdout
/// (blocked on a PTY read); taking the lock there would trade a broken
/// terminal for a wrap process that never exits. The cost of not locking:
/// this write can interleave with a concurrent read-loop chunk — including
/// landing mid-escape-sequence after a short `write(2)`, or ahead of bytes
/// still buffered between the loop's `write_all` and `flush` — garbling part
/// of the restore. Accepted: it only arises on the signal path of a process
/// that exits immediately after, and a partially-garbled restore attempt
/// still beats the deadlock.
#[cfg(unix)]
fn write_stdout_unlocked(bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: plain write(2) on fd 1 with an in-bounds slice.
        let rc = unsafe {
            libc::write(
                1,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
            )
        };
        if rc > 0 {
            written += rc as usize;
        } else if rc < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {
            continue;
        } else {
            break;
        }
    }
}

/// Without a signal path (no Unix signals), the restore only runs on the
/// main-thread drop path after the read loop released the lock, so the
/// ordinary locked stdout is safe here.
#[cfg(not(unix))]
fn write_stdout_unlocked(bytes: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(bytes);
    let _ = stdout.flush();
}

/// Handle a terminating signal delivered to wrap itself (external kill,
/// terminal-close HUP): forward the same signal to the child, restore the
/// terminal from the latched-mode state, and exit `128 + N`.
///
/// Keyboard Ctrl-C never lands here — raw mode delivers it to wrap as a
/// `0x03` byte that is forwarded to the child. Without this thread a signal
/// death would skip `Drop` entirely, leaking raw mode and every latched mode.
/// Runs on a normal thread via `signal_hook::iterator` (same pattern as
/// `sigwinch_loop`), so no async-signal-safety constraints apply.
///
/// Accepted race with the read loop: the filter reports a mode to the
/// tracker before the loop writes that chunk to stdout, so the snapshot
/// taken here can include an enable the terminal never received — for kitty
/// that direction means emitting a pop the terminal never saw pushed, which
/// can pop an enclosing context's entry. The window is the microseconds
/// between report and write inside a process being externally killed;
/// deferring reporting until after the write would flip the race to
/// under-restore but puts a per-CSI buffer on the hot output path.
#[cfg(unix)]
fn terminate_signal_loop(
    mut signals: signal_hook::iterator::Signals,
    tracker: Arc<ModeTracker>,
    child_pid: Option<u32>,
    child_reaped: Arc<std::sync::atomic::AtomicBool>,
) {
    if let Some(signal) = signals.forever().next() {
        // Skip the forward once the child is reaped: its pid is recyclable
        // and the kill could hit a bystander. The check narrows — but cannot
        // close — the reuse window (a reap can land between it and the
        // kill); that residual window is the same one every signal-forwarding
        // wrapper accepts.
        if let Some(pid) = child_pid
            && !child_reaped.load(std::sync::atomic::Ordering::SeqCst)
        {
            // Forward first so the child can run its own teardown while we
            // restore. Its late output goes to a PTY we are abandoning.
            // SAFETY: kill(2) has no memory-safety preconditions; pid is
            // positive (never the 0/-1 broadcast forms).
            unsafe { libc::kill(pid as libc::pid_t, signal) };
        }
        restore_terminal(&tracker);
        std::process::exit(128 + signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_restore_done_returns_immediately_when_finished() {
        let tracker = ModeTracker::new();
        assert!(tracker.begin_restore());
        tracker.finish_restore();
        assert!(wait_restore_done(
            &tracker,
            std::time::Duration::from_millis(100)
        ));
    }

    #[test]
    fn wait_restore_done_times_out_when_winner_never_finishes() {
        let tracker = ModeTracker::new();
        assert!(tracker.begin_restore());
        // No finish_restore: the bounded wait must give up, not hang.
        assert!(!wait_restore_done(
            &tracker,
            std::time::Duration::from_millis(5)
        ));
    }

    fn env_str(cmd: &portable_pty::CommandBuilder, key: &str) -> Option<String> {
        cmd.get_env(key)
            .and_then(|value| value.to_str().map(str::to_owned))
    }

    #[test]
    fn apply_wrap_child_env_dark_overrides_parent_light_on_both_names() {
        let mut cmd = portable_pty::CommandBuilder::new("true");
        cmd.env("ECHO_AGENT_APPEARANCE", "light");
        cmd.env("LC_ECHO_AGENT_APPEARANCE", "light");
        apply_wrap_child_env(&mut cmd, Some(SystemAppearance::Dark));
        assert_eq!(
            env_str(&cmd, "ECHO_AGENT_APPEARANCE").as_deref(),
            Some("dark")
        );
        assert_eq!(
            env_str(&cmd, "LC_ECHO_AGENT_APPEARANCE").as_deref(),
            Some("dark")
        );
        assert_eq!(env_str(&cmd, "ECHO_AGENT_OSC52_SINK").as_deref(), Some("1"));
        assert_eq!(
            env_str(&cmd, "LC_ECHO_AGENT_OSC52_SINK").as_deref(),
            Some("1")
        );
    }

    #[test]
    fn apply_wrap_child_env_light_overrides_parent_dark_on_both_names() {
        let mut cmd = portable_pty::CommandBuilder::new("true");
        cmd.env("ECHO_AGENT_APPEARANCE", "dark");
        cmd.env("LC_ECHO_AGENT_APPEARANCE", "dark");
        apply_wrap_child_env(&mut cmd, Some(SystemAppearance::Light));
        assert_eq!(
            env_str(&cmd, "ECHO_AGENT_APPEARANCE").as_deref(),
            Some("light")
        );
        assert_eq!(
            env_str(&cmd, "LC_ECHO_AGENT_APPEARANCE").as_deref(),
            Some("light")
        );
    }

    #[test]
    fn apply_wrap_child_env_none_does_not_stamp_from_parent_snapshot() {
        let mut cmd = portable_pty::CommandBuilder::new("true");
        cmd.env("ECHO_AGENT_APPEARANCE", "dark");
        cmd.env_remove("LC_ECHO_AGENT_APPEARANCE");
        apply_wrap_child_env(&mut cmd, None);
        assert_eq!(
            env_str(&cmd, "ECHO_AGENT_APPEARANCE").as_deref(),
            Some("dark")
        );
        assert_eq!(env_str(&cmd, "LC_ECHO_AGENT_APPEARANCE"), None);
        assert_eq!(env_str(&cmd, "ECHO_AGENT_OSC52_SINK").as_deref(), Some("1"));
    }
}
