//! Shared subprocess plumbing: spawn a child, optionally feed it stdin, wait up
//! to a wall-clock budget, and reap the whole process group on a breach.
//!
//! Used by both the optional [`crate::MmdcEngine`] (which shells out to
//! `mmdc`/headless-Chromium) and the pager's out-of-process render child (a
//! short-lived re-exec of the pager that renders one diagram in isolation). The
//! timeout is a *real* process kill, not a soft signal: a panic under
//! `panic = "abort"` or a runaway render in the child is contained because the
//! parent kills and reaps it.
//!
//! The caller builds the [`Command`] (stdio, env, and the sanctioned
//! TTY/console detach via `echo_agent_tty_utils::detach_std_command`); this module only
//! owns the spawn → feed-stdin → wait → reap lifecycle so neither call site
//! re-implements process-group teardown.

use std::process::{Child, Command};
use std::time::Duration;

use wait_timeout::ChildExt;

/// Why a child subprocess run did not complete successfully.
#[derive(thiserror::Error, Debug)]
pub enum SubprocessError {
    /// The child could not be spawned (binary missing, fork failure, …).
    #[error("could not spawn child process: {0}")]
    Spawn(std::io::Error),
    /// The child exceeded its wall-clock budget and was killed and reaped.
    #[error("child process timed out")]
    Timeout,
    /// The child ran to completion but exited non-zero.
    #[error("child process exited with {0}")]
    NonZeroExit(std::process::ExitStatus),
    /// Waiting on the child itself failed; the child was reaped defensively.
    #[error("waiting on child process failed: {0}")]
    Wait(std::io::Error),
}

/// Spawn `cmd`, optionally write `stdin_payload` to its stdin, wait up to
/// `timeout`, and reap the process group on a breach.
///
/// The caller must have configured `cmd` (stdio, env, detach). To pass
/// `stdin_payload`, the caller must set `cmd.stdin(Stdio::piped())`; the payload
/// is written from a scoped thread so a full pipe buffer can never deadlock the
/// wait. When `stdin_payload` is `None` (or stdin is not piped), no writer runs.
///
/// Returns `Ok(())` only on a zero-exit run; otherwise the matching
/// [`SubprocessError`]. On timeout or a failed wait the child is killed and
/// reaped: on Unix the whole process group is SIGKILLed, so grandchildren (e.g.
/// an [`crate::MmdcEngine`]'s headless Chromium) are reaped too; on Windows only
/// the direct child is killed — sufficient for the pager's render child (no
/// grandchildren), but a Windows `MmdcEngine` could leak Chromium grandchildren
/// (a Job Object is the follow-up there).
pub fn run_with_timeout(
    mut cmd: Command,
    stdin_payload: Option<&[u8]>,
    timeout: Duration,
) -> Result<(), SubprocessError> {
    let mut child = spawn_with_etxtbsy_retry(&mut cmd).map_err(SubprocessError::Spawn)?;

    // Feed stdin from a scoped thread so a child that stops reading can't wedge
    // a `write_all` of a large (up to the source-size cap) payload and deadlock
    // the wait below. On timeout we kill the child, which EOF/EPIPEs the writer.
    let stdin = child.stdin.take();

    // A payload with no piped stdin would be silently dropped (the caller forgot
    // `cmd.stdin(Stdio::piped())`). Both in-tree callers pipe correctly; flag the
    // foot-gun loudly in debug and at least log it in release.
    if stdin_payload.is_some() && stdin.is_none() {
        tracing::warn!(
            target: "mermaid",
            "run_with_timeout: stdin payload supplied but stdin is not piped; payload dropped"
        );
        debug_assert!(
            false,
            "run_with_timeout: stdin_payload supplied but cmd.stdin is not piped (payload dropped)"
        );
    }
    std::thread::scope(|scope| {
        if let (Some(mut sink), Some(payload)) = (stdin, stdin_payload) {
            scope.spawn(move || {
                use std::io::Write as _;
                // Errors are expected if the child exits/dies first; ignore them.
                let _ = sink.write_all(payload);
                // Dropping `sink` closes the pipe so the child observes EOF.
            });
        }
        wait_and_reap(&mut child, timeout)
    })
}

/// Spawn `cmd`, retrying briefly on `ETXTBSY` ("Text file busy").
///
/// On Linux, exec'ing a binary that another thread/process still holds open for
/// writing fails with `ExecutableFileBusy`. A concurrent `Command::spawn` on
/// another thread forks and inherits any write fd open at that instant; the fd
/// is close-on-exec but only closes at the child's own `execve`, leaving a
/// fork→execve window during which our `execve` of a freshly-written binary can
/// race. It is transient and clears within milliseconds, so retry a few times
/// with a short backoff. (No-op on the steady-state path; only the failing
/// transient case changes behaviour.)
#[allow(clippy::disallowed_methods)] // the caller owns the reap
fn spawn_with_etxtbsy_retry(cmd: &mut Command) -> std::io::Result<Child> {
    const MAX_ATTEMPTS: u32 = 5;
    let mut attempt = 0;
    loop {
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < MAX_ATTEMPTS =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(20 * attempt as u64));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Wait for `child` up to `timeout`, tearing down its detached process group on
/// every exit path (success, non-zero exit, timeout, wait failure) so a child
/// that spawned grandchildren can't orphan them.
fn wait_and_reap(child: &mut Child, timeout: Duration) -> Result<(), SubprocessError> {
    match child.wait_timeout(timeout) {
        // `wait_timeout` already reaped the direct child on these two branches,
        // but it was its own detached group leader: SIGKILL the group's pgid so
        // any grandchildren (e.g. an opt-in MmdcEngine's headless Chromium) are
        // torn down regardless of exit code. The pgid stays valid while a
        // grandchild is alive (the case that matters); for the grandchild-less
        // render child the leader is already gone, so killpg is a harmless no-op
        // (ESRCH). The full `reap()` is unneeded — the direct child is already
        // reaped, so `child.kill()`/`wait()` would be redundant.
        Ok(Some(status)) if status.success() => {
            reap_process_group(child);
            Ok(())
        }
        Ok(Some(status)) => {
            reap_process_group(child);
            Err(SubprocessError::NonZeroExit(status))
        }
        Ok(None) => {
            reap(child);
            Err(SubprocessError::Timeout)
        }
        // waitpid failed; the child may still be running, so reap it too — the
        // same teardown as the timeout branch (don't leak the child tree).
        Err(e) => {
            reap(child);
            Err(SubprocessError::Wait(e))
        }
    }
}

/// Best-effort teardown of a spawned child: SIGKILL its process group (to reach
/// any grandchildren, e.g. headless Chromium), then kill and reap the child.
fn reap(child: &mut Child) {
    reap_process_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// SIGKILL the child's process group so grandchildren are reaped, not just the
/// direct child.
///
/// `echo_agent_tty_utils::detach_std_command` runs `setsid` (EPERM fallback
/// `setpgid(0,0)`), so the child is its own group leader and its pgid equals its
/// pid. We send the signal directly because `echo_agent_tty_utils::ProcessGroup` only
/// wraps tokio children.
#[cfg(unix)]
fn reap_process_group(child: &Child) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: killpg with a valid pid + standard signal has no memory effects.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn reap_process_group(_child: &Child) {
    // Group teardown via Job Objects is tokio-only here; the caller's
    // `child.kill()` still terminates the direct child process.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Instant;

    fn detached(mut cmd: Command) -> Command {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd
    }

    #[cfg(unix)]
    #[test]
    fn zero_exit_is_ok() {
        let cmd = detached(Command::new("true"));
        assert!(run_with_timeout(cmd, None, Duration::from_secs(5)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn nonzero_exit_is_reported() {
        let cmd = detached(Command::new("false"));
        let r = run_with_timeout(cmd, None, Duration::from_secs(5));
        assert!(
            matches!(r, Err(SubprocessError::NonZeroExit(_))),
            "got {r:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn slow_command_times_out_quickly() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let cmd = detached(cmd);
        let start = Instant::now();
        let r = run_with_timeout(cmd, None, Duration::from_millis(150));
        assert!(matches!(r, Err(SubprocessError::Timeout)));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "should return at the deadline, not wait the full 5s",
        );
    }

    /// A large stdin payload (bigger than any OS pipe buffer) must be delivered
    /// through `run_with_timeout`'s own scoped writer without deadlocking the
    /// wait — the whole reason the writer is a scoped thread. We point `cat` at a
    /// file (its stdout) so we can prove every byte was consumed, and assert the
    /// call returns `Ok(())` promptly rather than hitting the timeout.
    #[cfg(unix)]
    #[test]
    fn large_stdin_payload_is_delivered_without_deadlock() {
        let payload = vec![b'x'; 256 * 1024];
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = dir.path().join("drained");
        let sink_file = std::fs::File::create(&sink).expect("create sink");

        let mut cmd = Command::new("cat");
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::from(sink_file))
            .stderr(Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);

        let start = Instant::now();
        let r = run_with_timeout(cmd, Some(&payload), Duration::from_secs(10));
        assert!(
            r.is_ok(),
            "draining a large stdin payload must succeed: {r:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must return after the drain, not after the full timeout",
        );
        // `cat` copies all of stdin to the file → proves the whole payload was
        // both delivered and consumed via the real scoped writer.
        let drained = std::fs::metadata(&sink).expect("sink metadata").len();
        assert_eq!(
            drained,
            payload.len() as u64,
            "all stdin bytes round-tripped through cat"
        );
    }

    /// `reap` actually terminates the spawned process group.
    #[cfg(unix)]
    #[test]
    fn reap_terminates_the_process() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let mut cmd = detached(cmd);
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id() as libc::pid_t;

        reap(&mut child);

        // After SIGKILL + wait, the pid no longer names a live process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "process {pid} should be gone after reap",
        );
    }

    #[test]
    fn missing_binary_is_spawn_error() {
        let cmd = Command::new("definitely-not-a-real-binary-9f8a7b6c5d4e");
        let r = run_with_timeout(cmd, None, Duration::from_secs(5));
        assert!(matches!(r, Err(SubprocessError::Spawn(_))), "got {r:?}");
    }

    /// The dropped-stdin-payload guard fires when a caller passes a payload but
    /// forgets `cmd.stdin(Stdio::piped())`: the `debug_assert!` turns that silent
    /// foot-gun into a hard failure. Gated on `debug_assertions` because that is
    /// exactly when the assert is active (release keeps only the `warn`). `true`
    /// exits at once; `detached` sets stdin to null (not piped), so the payload
    /// would be dropped and the guard must catch it.
    #[cfg(all(unix, debug_assertions))]
    #[test]
    #[should_panic(expected = "stdin_payload supplied but cmd.stdin is not piped")]
    fn stdin_payload_without_piped_stdin_is_flagged() {
        let cmd = detached(Command::new("true"));
        let _ = run_with_timeout(cmd, Some(b"payload"), Duration::from_secs(5));
    }
}
