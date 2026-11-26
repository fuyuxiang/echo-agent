//! Process-local single-flight for the blocking flock wait: at most one OS thread
//! parks in the kernel per lock file, shared by all callers — without the dedupe,
//! every timed-out caller leaves its own thread parked against tokio's bounded
//! blocking pool. Liveness is ownership: the parked thread's deposit guard and
//! the tickets hold the only strong references, an unclaimed deposit is freed
//! when the last one drops, and the registry's `Weak` entries can neither
//! outlive nor poison a wait.

#[cfg(all(feature = "loom", not(test)))]
compile_error!("the `loom` feature is test-only: it swaps this module's mutexes for loom models");

#[cfg(all(test, feature = "loom"))]
#[path = "flock_wait_loom_tests.rs"]
mod loom_tests;

#[cfg(all(test, not(feature = "loom")))]
#[path = "flock_wait_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

#[cfg(feature = "loom")]
use loom::sync::{Mutex, MutexGuard};
#[cfg(not(feature = "loom"))]
use std::sync::{Mutex, MutexGuard};

// Process-global: every `AuthManager` in the process must share one wait per lock path.
// Lock order: WAITS -> round; every other site takes round only.
static WAITS: LazyLock<Mutex<HashMap<PathBuf, Weak<Wait>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `Deposited`/`Claimed` imply the parked thread has exited; losers seeing
/// `Claimed` rejoin.
enum Round {
    Waiting,
    Deposited(io::Result<File>),
    Claimed,
}

impl Round {
    fn take_deposit(&mut self) -> Option<io::Result<File>> {
        match std::mem::replace(self, Round::Claimed) {
            Round::Deposited(result) => Some(result),
            Round::Waiting => {
                *self = Round::Waiting;
                None
            }
            Round::Claimed => None,
        }
    }
}

struct Wait {
    round: Mutex<Round>,
    notify: tokio::sync::Notify,
}

impl Wait {
    fn lock_round(&self) -> MutexGuard<'_, Round> {
        self.round.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Subscription to a [`Wait`]; dropping it unsubscribes.
#[must_use]
pub(super) struct Ticket {
    wait: Arc<Wait>,
}

impl Ticket {
    /// Takes an already-deposited outcome without waiting.
    pub(super) fn try_claim(&self) -> Option<io::Result<File>> {
        self.wait.lock_round().take_deposit()
    }

    /// `Some` claims this round's outcome; `None` means another subscriber won — rejoin.
    pub(super) async fn claim(&self) -> Option<io::Result<File>> {
        loop {
            // Register before checking state so a racing deposit is not missed.
            let notified = self.wait.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut round = self.wait.lock_round();
                if let Some(result) = round.take_deposit() {
                    return Some(result);
                }
                if matches!(*round, Round::Claimed) {
                    return None;
                }
            }
            notified.await;
        }
    }
}

/// Deposits the acquire outcome and wakes waiters when dropped — on unwind too,
/// where the missing result becomes the deposited error.
struct DepositOnDrop {
    wait: Arc<Wait>,
    result: Option<io::Result<File>>,
}

impl Drop for DepositOnDrop {
    fn drop(&mut self) {
        let result = self
            .result
            .take()
            .unwrap_or_else(|| Err(io::Error::other("flock wait panicked")));
        *self.wait.lock_round() = Round::Deposited(result);
        self.wait.notify.notify_waiters();
    }
}

/// Subscribes to `entry` only if it is a live wait still parked on the flock.
fn subscribe_if_waiting(entry: &Weak<Wait>) -> Option<Ticket> {
    entry
        .upgrade()
        .filter(|wait| matches!(*wait.lock_round(), Round::Waiting))
        .map(|wait| Ticket { wait })
}

/// Subscribes to the live wait for `lock_path`, starting one only if none is waiting.
pub(super) fn join(lock_path: &Path) -> Ticket {
    let mut waits = WAITS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ticket) = waits.get(lock_path).and_then(subscribe_if_waiting) {
        return ticket;
    }
    let wait = Arc::new(Wait {
        round: Mutex::new(Round::Waiting),
        notify: tokio::sync::Notify::new(),
    });
    waits.retain(|_, entry| entry.strong_count() > 0);
    waits.insert(lock_path.to_owned(), Arc::downgrade(&wait));
    let deposit_wait = Arc::clone(&wait);
    let thread_path = lock_path.to_owned();
    let _detached_from_any_caller = tokio::task::spawn_blocking(move || {
        let mut deposit = DepositOnDrop {
            wait: deposit_wait,
            result: None,
        };
        deposit.result = Some(super::blocking_acquire(&thread_path));
    });
    Ticket { wait }
}
