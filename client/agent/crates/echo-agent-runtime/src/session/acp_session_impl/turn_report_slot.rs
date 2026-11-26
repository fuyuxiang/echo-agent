//! The two things a turn may only do once: report its end, and announce its abort. Both need
//! guarding because the stop gate runs on the turn task while a cancel runs on the command loop.

use std::cell::RefCell;

/// Bumped when a turn is promoted. A report carrying an older epoch is refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TurnEpoch(u64);

/// Only a gate claim is held across an await, so only a gate claim can be released by an abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaimKind {
    Gate,
    Report,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnReportState {
    #[default]
    Free,
    Held {
        claim: u64,
        kind: ClaimKind,
    },
    Reported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a lost claim means another reporter already reported this turn"]
pub(super) enum CommitOutcome {
    Reported,
    LostToAnotherReporter,
}

/// Which turn may still report its end, and who holds the right to. No borrow of `inner` outlives
/// a statement, so none can outlive an await.
#[derive(Debug, Default)]
pub(crate) struct TurnReportSlot {
    inner: RefCell<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    state: TurnReportState,
    epoch: TurnEpoch,
    /// Never reset, so a claim id is unique for the life of the session.
    next_claim: u64,
}

pub(super) struct TurnReportClaim<'a> {
    slot: &'a TurnReportSlot,
    claim: u64,
    committed: bool,
}

impl TurnReportClaim<'_> {
    pub(super) fn commit(mut self) -> CommitOutcome {
        self.committed = true;
        if self.slot.finish(self.claim, TurnReportState::Reported) {
            CommitOutcome::Reported
        } else {
            CommitOutcome::LostToAnotherReporter
        }
    }
}

impl Drop for TurnReportClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.slot.finish(self.claim, TurnReportState::Free);
        }
    }
}

impl TurnReportSlot {
    pub(super) fn epoch(&self) -> TurnEpoch {
        self.inner.borrow().epoch
    }

    /// Held across the user's hooks, and so across awaits.
    pub(super) fn claim_for_gate(&self) -> Option<TurnReportClaim<'_>> {
        self.try_claim(self.epoch(), ClaimKind::Gate)
    }

    pub(super) fn claim_at(&self, epoch: TurnEpoch) -> Option<TurnReportClaim<'_>> {
        self.try_claim(epoch, ClaimKind::Report)
    }

    fn try_claim(&self, epoch: TurnEpoch, kind: ClaimKind) -> Option<TurnReportClaim<'_>> {
        let mut inner = self.inner.borrow_mut();
        if inner.epoch != epoch || inner.state != TurnReportState::Free {
            return None;
        }
        inner.next_claim += 1;
        let claim = inner.next_claim;
        inner.state = TurnReportState::Held { claim, kind };
        Some(TurnReportClaim {
            slot: self,
            claim,
            committed: false,
        })
    }

    /// Only from `abort_turn_task`: frees the gate claim belonging to the task it killed.
    pub(super) fn release_aborted(&self, epoch: TurnEpoch) {
        let mut inner = self.inner.borrow_mut();
        if inner.epoch == epoch
            && matches!(
                inner.state,
                TurnReportState::Held {
                    kind: ClaimKind::Gate,
                    ..
                }
            )
        {
            inner.state = TurnReportState::Free;
        }
    }

    pub(super) fn start_next_turn(&self) {
        let mut inner = self.inner.borrow_mut();
        if matches!(inner.state, TurnReportState::Held { .. }) {
            let held = "a turn started while the previous turn's report claim was still held";
            tracing::error!("{held}");
            debug_assert!(false, "{held}");
        }
        inner.epoch.0 += 1;
        inner.state = TurnReportState::Free;
    }

    #[cfg(test)]
    pub(super) fn state(&self) -> TurnReportState {
        self.inner.borrow().state
    }

    fn finish(&self, claim: u64, next: TurnReportState) -> bool {
        let mut inner = self.inner.borrow_mut();
        if !matches!(inner.state, TurnReportState::Held { claim: held, .. } if held == claim) {
            return false;
        }
        inner.state = next;
        true
    }
}

/// Announces a turn's abort once. A high-water mark rather than a match, because a cancel that
/// finishes after a newer turn started must not repeat its own nor consume the successor's.
#[derive(Debug, Default)]
pub(crate) struct AbortAnnouncement {
    announced: RefCell<Option<TurnEpoch>>,
}

impl AbortAnnouncement {
    pub(super) fn try_mark_announced(&self, epoch: TurnEpoch) -> bool {
        let mut announced = self.announced.borrow_mut();
        if announced.is_some_and(|seen| seen >= epoch) {
            return false;
        }
        *announced = Some(epoch);
        true
    }
}

#[cfg(test)]
#[path = "turn_report_slot_tests.rs"]
mod tests;
