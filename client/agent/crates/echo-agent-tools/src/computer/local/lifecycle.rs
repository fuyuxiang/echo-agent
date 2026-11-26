//! Where a task sits between running and evicted, and the proof required to
//! move it. The transitions live here, away from the process plumbing, so the
//! state machine can be tested without a live child.

use std::time::Instant;

use super::ExitStatus;

/// Proof of whether the child was waited on. The field is private:
/// [`Collection::of`] reads the child handle, which tokio clears once a
/// `wait` or `try_wait` has returned, so a call site cannot claim a wait
/// that never happened. [`Collection::ABANDONED`] is always claimable,
/// since that direction only costs more polling.
pub(super) struct Collection(bool);

impl Collection {
    pub(super) const ABANDONED: Collection = Collection(false);

    pub(super) fn of(child: &tokio::process::Child) -> Collection {
        Collection(child.id().is_none())
    }
}

/// Each stage carries only what it can have, so a task cannot be collected
/// before it exits, or hold a sweep time before its output is final.
#[derive(Debug, Clone)]
pub(super) enum Lifecycle {
    Running,
    /// Over, but the pipes still have to be read.
    Exiting {
        status: ExitStatus,
        since: Instant,
    },
    /// Output is final. A process that will not die reaches this uncollected.
    Finished {
        status: ExitStatus,
        collected: bool,
    },
    /// The in-memory copy has been dropped for the log on disk. An
    /// uncollected child keeps being polled after the sweep, until eviction
    /// abandons it.
    Swept {
        status: ExitStatus,
        at: Instant,
        collected: bool,
    },
}

impl Lifecycle {
    pub(super) fn exit_status(&self) -> Option<&ExitStatus> {
        match self {
            Self::Running => None,
            Self::Exiting { status, .. }
            | Self::Finished { status, .. }
            | Self::Swept { status, .. } => Some(status),
        }
    }

    pub(super) fn has_exited(&self) -> bool {
        self.exit_status().is_some()
    }

    /// Over, with all of its output read.
    pub(super) fn is_complete(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Swept { .. })
    }

    /// Nothing left for the poll loop: complete *and* the child was waited
    /// on. Sweeping does not settle a task on its own.
    pub(super) fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Finished {
                collected: true,
                ..
            } | Self::Swept {
                collected: true,
                ..
            }
        )
    }

    pub(super) fn swept_at(&self) -> Option<Instant> {
        match self {
            Self::Running | Self::Exiting { .. } | Self::Finished { .. } => None,
            Self::Swept { at, .. } => Some(*at),
        }
    }

    /// Output is final. A late collection upgrades a finished or swept task
    /// in place; nothing moves back a stage. No-op before the task exits.
    pub(super) fn finish_output(&mut self, collection: Collection) {
        let Some(status) = self.exit_status().cloned() else {
            return;
        };
        let collected = collection.0;
        *self = match self {
            Self::Swept {
                at,
                collected: already,
                ..
            } => Self::Swept {
                status,
                at: *at,
                collected: *already || collected,
            },
            Self::Finished {
                collected: already, ..
            } => Self::Finished {
                status,
                collected: *already || collected,
            },
            Self::Running | Self::Exiting { .. } => Self::Finished { status, collected },
        };
    }

    /// Drops to the log on disk. Only a finished task can be swept, and
    /// `collected` carries over.
    pub(super) fn sweep(&mut self) {
        if let Self::Finished { status, collected } = self {
            *self = Self::Swept {
                status: status.clone(),
                at: Instant::now(),
                collected: *collected,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Collection, ExitStatus, Lifecycle};
    use std::time::Instant;

    fn exiting() -> Lifecycle {
        Lifecycle::Exiting {
            status: ExitStatus {
                exit_code: None,
                signal: Some("timeout".to_owned()),
            },
            since: Instant::now(),
        }
    }

    /// The walk the out-of-memory and give-up kill paths take. Both once
    /// settled a task whose child was never collected; every step here pins
    /// the boundary they crossed.
    #[test]
    fn a_kill_without_a_reap_keeps_the_task_polled_until_collected() {
        let mut lifecycle = exiting();
        assert!(lifecycle.has_exited());
        assert!(!lifecycle.is_complete(), "exited is not yet complete");

        lifecycle.finish_output(Collection::ABANDONED);
        assert!(lifecycle.is_complete(), "output is final, so waits answer");
        assert!(!lifecycle.is_settled(), "the child still needs a try_wait");

        lifecycle.sweep();
        assert!(!lifecycle.is_settled(), "sweeping must not end the polling");

        lifecycle.finish_output(Collection(true));
        assert!(lifecycle.is_settled(), "the late reap settles it");
        assert!(lifecycle.swept_at().is_some(), "and it stays swept");
    }

    /// Output still being read must not be dropped.
    #[test]
    fn a_task_still_draining_cannot_be_swept() {
        let mut lifecycle = exiting();

        lifecycle.sweep();

        assert!(lifecycle.swept_at().is_none());
    }

    /// The evidence reads the child handle: no collection can be claimed
    /// until a `wait` has returned.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_collection_claim_requires_the_child_to_have_been_waited_on() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let mut lifecycle = exiting();

        lifecycle.finish_output(Collection::of(&child));
        assert!(!lifecycle.is_settled(), "no wait has returned");

        child.wait().await.expect("wait");
        lifecycle.finish_output(Collection::of(&child));
        assert!(lifecycle.is_settled(), "the wait is the evidence");
    }
}
