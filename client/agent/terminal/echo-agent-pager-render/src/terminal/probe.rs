//! Shared startup terminal-probe primitive: write a query, and (OSC 11
//! only) raw-fd poll/read stdin until a terminator or deadline.
//! XTVERSION uses only `write_query`;
//! its reply is handled by the event loop's response filter.
//!
//! Safety invariants (timed-read path):
//! - Startup-only: must run before crossterm's `EventStream` exists (both
//!   compete for stdin).
//! - Keystrokes typed inside the read window are consumed and dropped — no
//!   portable re-injection exists (TIOCSTI is blocked); accepted loss.

use std::io::Write;
use std::time::Duration;

/// Bounds the reply buffer against terminals that stream without a terminator.
#[cfg(unix)]
pub(crate) const MAX_PROBE_RESPONSE: usize = 256;

/// Hard cap on post-deadline consumption of an in-flight reply.
#[cfg(unix)]
const LATE_REPLY_GRACE: Duration = Duration::from_millis(100);

/// Per-byte quiet window during the grace period.
#[cfg(unix)]
const LATE_REPLY_QUIET_MS: i32 = 25;

/// Write a probe query via the shared stderr lock; `false` if the TUI fd is
/// not a TTY or the write fails.
pub(crate) fn write_query(query: &[u8]) -> bool {
    use std::io::IsTerminal;

    let write_result: std::io::Result<()> =
        echo_agent_shared::stderr::with_locked_stderr(|stderr| {
            // fd 2 is /dev/null-redirected; the TTY check must run on the
            // dup'd render fd inside the lock, not on std::io::stderr().
            if !stderr.is_terminal() {
                return Err(std::io::Error::other("TUI output is not a TTY"));
            }
            stderr.write_all(query)?;
            stderr.flush()
        });
    write_result.is_ok()
}

/// Read stdin until `is_terminated`, the size cap, or the deadline.
///
/// Returns `Some(buf)` whenever bytes were consumed (even partial, so a
/// half-read reply is never left for the EventStream); `None` when nothing
/// arrived or stdin errored before any byte.
#[cfg(unix)]
pub(crate) fn read_tty_reply(
    timeout: Duration,
    mut is_terminated: impl FnMut(&[u8], u8) -> bool,
) -> Option<Vec<u8>> {
    use std::os::unix::io::AsRawFd;

    let fd = std::io::stdin().as_raw_fd();
    let start = std::time::Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(64);

    loop {
        let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
            return finish_after_deadline(fd, buf, is_terminated);
        };
        let remaining_ms = remaining.as_millis().min(i32::MAX as u128) as i32;

        match poll_read_byte(fd, remaining_ms) {
            PollRead::Byte(byte) => {
                buf.push(byte);
                if buf.len() >= MAX_PROBE_RESPONSE || is_terminated(&buf, byte) {
                    return Some(buf);
                }
            }
            // Re-entry recomputes the deadline, so EINTR cannot extend it.
            PollRead::Interrupted => continue,
            PollRead::Timeout => return finish_after_deadline(fd, buf, is_terminated),
            PollRead::Error => return if buf.is_empty() { None } else { Some(buf) },
        }
    }
}

/// Deadline expiry: an in-flight reply (ESC byte seen — replies are
/// DCS/CSI/OSC, plain keystrokes aren't) is consumed until quiet so its
/// tail can't reach the EventStream as typed garbage; otherwise return
/// immediately to avoid eating keystrokes at a silent terminal.
#[cfg(unix)]
fn finish_after_deadline(
    fd: i32,
    mut buf: Vec<u8>,
    mut is_terminated: impl FnMut(&[u8], u8) -> bool,
) -> Option<Vec<u8>> {
    if buf.is_empty() {
        return None;
    }
    if !buf.contains(&0x1b) {
        return Some(buf);
    }
    let grace_start = std::time::Instant::now();
    while grace_start.elapsed() < LATE_REPLY_GRACE {
        match poll_read_byte(fd, LATE_REPLY_QUIET_MS) {
            PollRead::Byte(byte) => {
                buf.push(byte);
                if buf.len() >= MAX_PROBE_RESPONSE || is_terminated(&buf, byte) {
                    break;
                }
            }
            PollRead::Interrupted => continue,
            PollRead::Timeout | PollRead::Error => break,
        }
    }
    Some(buf)
}

#[cfg(unix)]
enum PollRead {
    Byte(u8),
    Interrupted,
    Timeout,
    Error,
}

/// One EINTR-retrying poll-then-read step for a single byte.
#[cfg(unix)]
fn poll_read_byte(fd: i32, timeout_ms: i32) -> PollRead {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is a valid pollfd struct with a valid fd.
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if ret == 0 {
        return PollRead::Timeout;
    }
    if ret < 0 {
        return if last_errno_is_eintr() {
            PollRead::Interrupted
        } else {
            PollRead::Error
        };
    }

    loop {
        let mut byte = [0u8; 1];
        // SAFETY: byte is a valid buffer of length 1.
        let n = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if n == 1 {
            return PollRead::Byte(byte[0]);
        }
        if n < 0 && last_errno_is_eintr() {
            continue;
        }
        return PollRead::Error;
    }
}

#[cfg(unix)]
fn last_errno_is_eintr() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
}
