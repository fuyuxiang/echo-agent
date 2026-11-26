//! Process-wide libgit2/git ODB permit.
//!
//! libgit2 walks serialize on an in-process mutex and this permit. CLI status
//! is only in the cap to bound pack I/O when uncontended. Prompt CLI may run
//! without a permit under contention so `<git_status>` is not dropped behind a
//! long acquire wait.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const ODB_PERMITS: usize = 1;
pub const WALK_TIMEOUT_SECS: u64 = 60;
pub const ODB_ACQUIRE_SLACK_SECS: u64 = 5;
pub const WALK_TIMEOUT: Duration = Duration::from_secs(WALK_TIMEOUT_SECS);
pub const ODB_ACQUIRE_WAIT: Duration =
    Duration::from_secs(WALK_TIMEOUT_SECS + ODB_ACQUIRE_SLACK_SECS);

#[derive(Clone)]
pub struct OdbLimiter {
    inner: Arc<OdbLimiterInner>,
}

struct OdbLimiterInner {
    sem: Arc<Semaphore>,
    acquire_wait: Duration,
}

#[must_use]
pub struct OdbPermit {
    _permit: OwnedSemaphorePermit,
}

static PROCESS_ODB: LazyLock<OdbLimiter> =
    LazyLock::new(|| OdbLimiter::new(ODB_PERMITS, ODB_ACQUIRE_WAIT));

pub fn shared() -> &'static OdbLimiter {
    &PROCESS_ODB
}

pub fn try_acquire_odb() -> Option<OdbPermit> {
    PROCESS_ODB.try_acquire()
}

impl OdbLimiter {
    #[must_use]
    pub fn new(permits: usize, acquire_wait: Duration) -> Self {
        Self {
            inner: Arc::new(OdbLimiterInner {
                sem: Arc::new(Semaphore::new(permits.max(1))),
                acquire_wait,
            }),
        }
    }

    pub async fn acquire(&self) -> Result<OdbPermit> {
        match tokio::time::timeout(
            self.inner.acquire_wait,
            self.inner.sem.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(OdbPermit { _permit: permit }),
            Ok(Err(_)) => Err(anyhow!("git object database semaphore closed")),
            Err(_) => Err(anyhow!(
                "timed out waiting {}s for git object database permit",
                self.inner.acquire_wait.as_secs()
            )),
        }
    }

    pub fn try_acquire(&self) -> Option<OdbPermit> {
        self.inner
            .sem
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| OdbPermit { _permit: permit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_is_none_while_held_and_some_when_free() {
        let limiter = OdbLimiter::new(1, Duration::from_secs(1));
        let held = limiter.try_acquire();
        assert!(held.is_some());
        assert!(limiter.try_acquire().is_none());
        drop(held);
        assert!(limiter.try_acquire().is_some());
    }
}
