//! Signal-free bounded waiting for synchronous child processes.

use std::io;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

// Forty milliseconds balances responsive exit detection against polling load.
const CHILD_EXIT_POLL_QUANTUM: Duration = Duration::from_millis(40);

/// Poll `child` for up to one monotonic `timeout`, reaping it on exit.
///
/// `Some` means reaped; `None` leaves the child running and caller-owned. Errors
/// perform no cleanup. Unix `ECHILD` makes numeric PID/process-group identity
/// uncertain, so callers must not signal either. A zero timeout still performs
/// initial and final deadline polls.
///
/// # Errors
///
/// Returns the first error from [`Child::try_wait`].
pub fn wait_child_bounded(child: &mut Child, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    // Not thread::sleep: a foreign SIGCHLD handler can overwrite EINTR and trip its errno assert (GB-5008).
    // Not park_timeout: that would consume this thread's park token.
    let mutex = Mutex::new(());
    let condvar = Condvar::new();
    wait_child_bounded_with(
        timeout,
        || child.try_wait(),
        || started.elapsed(),
        |duration| {
            let guard = mutex.lock().unwrap_or_else(PoisonError::into_inner);
            let _guard = condvar
                .wait_timeout_while(guard, duration, |_| true)
                .unwrap_or_else(PoisonError::into_inner);
        },
    )
}

/// Transfer child/group ownership to a named eventual reaper, or return both.
///
/// # Errors
///
/// Returns thread-spawn failure together with the untransferred owners.
pub fn spawn_child_reaper(
    name: &str,
    child: Child,
    group: Option<Arc<crate::ProcessGroup>>,
) -> Result<(), (io::Error, Child, Option<Arc<crate::ProcessGroup>>)> {
    let owners = Arc::new(Mutex::new(Some((child, group))));
    let thread_owners = Arc::clone(&owners);
    match std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let Some((mut child, group)) = thread_owners
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            else {
                return;
            };
            let _ = child.wait();
            drop(group);
        }) {
        Ok(_) => Ok(()),
        Err(error) => {
            let owners = owners.lock().unwrap_or_else(PoisonError::into_inner).take();
            match owners {
                Some((child, group)) => Err((error, child, group)),
                #[allow(
                    clippy::unreachable,
                    reason = "a failed reaper spawn cannot also hold ownership taken by the thread"
                )]
                None => unreachable!("failed thread cannot take reaper ownership"),
            }
        }
    }
}

/// Whether a wait error means a numeric Unix child/group identity may be stale.
pub fn is_child_wait_identity_uncertain(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ECHILD)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

fn wait_child_bounded_with<P, E, W>(
    timeout: Duration,
    mut poll: P,
    mut elapsed: E,
    mut wait: W,
) -> io::Result<Option<ExitStatus>>
where
    P: FnMut() -> io::Result<Option<ExitStatus>>,
    E: FnMut() -> Duration,
    W: FnMut(Duration),
{
    if let Some(status) = poll()? {
        return Ok(Some(status));
    }

    loop {
        let remaining = timeout.saturating_sub(elapsed());
        if remaining.is_zero() {
            return poll();
        }
        wait(remaining.min(CHILD_EXIT_POLL_QUANTUM));
        if timeout.saturating_sub(elapsed()).is_zero() {
            return poll();
        }
        if let Some(status) = poll()? {
            return Ok(Some(status));
        }
    }
}

#[cfg(test)]
#[path = "child_wait_tests.rs"]
mod tests;
