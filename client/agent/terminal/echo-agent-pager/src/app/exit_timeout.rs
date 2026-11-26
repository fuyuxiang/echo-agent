//! Forces process exit when a graceful quit does not finish.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// From the earliest arm site (a signal, before loop unwind): the agent
// join bound (SESSION_FLUSH_GRACE + slack, 12s) plus flushes and headroom.
const DEFAULT_EXIT_TIMEOUT: Duration = Duration::from_secs(20);
const EXIT_TIMEOUT_ENV: &str = "ECHO_AGENT_EXIT_TIMEOUT_SECS";
const HARD_EXIT_GRACE: Duration = Duration::from_secs(5);
const TEST_HOLD_TEARDOWN_ENV: &str = "ECHO_AGENT_TEST_HOLD_TEARDOWN_SECS";

static ARMED: AtomicBool = AtomicBool::new(false);

/// Arm the exit timeout. The first caller to spawn fixes the exit code;
/// a failed spawn un-latches so a later quit path can retry.
pub(crate) fn arm(exit_code: i32) {
    if cfg!(test) {
        return;
    }
    if ARMED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(timeout) = parse_timeout(std::env::var(EXIT_TIMEOUT_ENV).ok().as_deref()) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("exit-timeout".into())
        .spawn(move || {
            std::thread::sleep(timeout);
            spawn_hard_exit(exit_code);
            super::signal_handler::force_exit(exit_code);
        });
    if spawned.is_err() {
        ARMED.store(false, Ordering::Release);
    }
}

/// After the grace, exit without touching any user-space lock. If the
/// backup thread cannot spawn, exit here: the guarantee outranks the
/// polite teardown the caller was about to attempt.
fn spawn_hard_exit(exit_code: i32) {
    let spawned = std::thread::Builder::new()
        .name("quit-hard-exit".into())
        .spawn(move || {
            std::thread::sleep(HARD_EXIT_GRACE);
            hard_exit(exit_code);
        });
    if spawned.is_err() {
        hard_exit(exit_code);
    }
}

fn hard_exit(exit_code: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` is async-signal-safe and skips atexit handlers.
    unsafe {
        libc::_exit(exit_code)
    };
    #[cfg(not(unix))]
    std::process::exit(exit_code);
}

/// Hold teardown for [`TEST_HOLD_TEARDOWN_ENV`] seconds; a no-op when unset.
pub(crate) fn hold_teardown_for_test() {
    if let Some(secs) = std::env::var(TEST_HOLD_TEARDOWN_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

fn parse_timeout(env: Option<&str>) -> Option<Duration> {
    match env.map(str::trim).and_then(|v| v.parse::<u64>().ok()) {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(DEFAULT_EXIT_TIMEOUT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timeout_cases() {
        assert_eq!(parse_timeout(None), Some(DEFAULT_EXIT_TIMEOUT));
        assert_eq!(parse_timeout(Some(" 2 ")), Some(Duration::from_secs(2)));
        assert_eq!(parse_timeout(Some("0")), None);
        assert_eq!(parse_timeout(Some("fast")), Some(DEFAULT_EXIT_TIMEOUT));
    }
}
