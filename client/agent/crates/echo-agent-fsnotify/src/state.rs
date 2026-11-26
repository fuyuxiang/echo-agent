//! Lock-state machine. Pure data + pure transition function. No I/O.

use std::time::{Duration, Instant};

/// Drop transient OS events for this window after a head-changing op;
/// consumers refresh from scratch anyway.
pub(crate) const COOLDOWN_MS: u64 = 500;

/// After a lock release, wait this long before declaring the operation
/// complete: a lock reappearing within the window (a rebase/squash cycles
/// `index.lock` per pick) is the *same* operation, so rapid cycles merge into
/// one `Started`/`Completed` pair instead of storming consumers.
pub const SETTLE_MS: u64 = 500;

/// Diagnostic threshold — fires a one-time warning when a lock is held
/// longer than this. `git gc` on huge repos can exceed this legitimately;
/// the state machine stays locked until the lock file disappears regardless.
const STALE_LOCK_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LockState {
    Idle,
    Locked {
        head_at_start: Option<String>,
        since: Instant,
    },
    /// Lock released, operation not yet declared complete. `head_at_start`
    /// and `since` are carried from the first `Locked` entry of the merged
    /// operation so re-locks preserve the op-wide HEAD comparison and the
    /// stale-lock clock.
    Settling {
        head_at_start: Option<String>,
        since: Instant,
        until: Instant,
    },
    Cooldown {
        until: Instant,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LockTransition {
    None,
    Started,
    /// Emitted on any `Locked → !Locked` transition. `head_changed` is the
    /// HEAD comparison; cooldown begins iff true.
    Completed {
        head_changed: bool,
    },
    /// Cooldown timer expired; consumer never sees this — internal only.
    CooldownEnded,
}

/// One step. Pure; mutates `state` from freshly-observed FS facts.
pub(crate) fn drive(
    state: &mut LockState,
    lock_present: bool,
    head_now: Option<String>,
    now: Instant,
    cooldown: Duration,
) -> LockTransition {
    match (state.clone(), lock_present) {
        (LockState::Idle, true) | (LockState::Cooldown { .. }, true) => {
            *state = LockState::Locked {
                head_at_start: head_now,
                since: now,
            };
            LockTransition::Started
        }
        // Same operation resumes: keep the op-start HEAD and `since` so the
        // eventual Completed spans the whole merged op. No duplicate Started —
        // consumers never saw a Completed, so their in-op flag never flipped.
        (
            LockState::Settling {
                head_at_start,
                since,
                ..
            },
            true,
        ) => {
            *state = LockState::Locked {
                head_at_start,
                since,
            };
            LockTransition::None
        }
        // Don't complete yet: give a rapid re-lock the settle window to merge.
        (
            LockState::Locked {
                head_at_start,
                since,
            },
            false,
        ) => {
            *state = LockState::Settling {
                head_at_start,
                since,
                until: now + Duration::from_millis(SETTLE_MS),
            };
            LockTransition::None
        }
        (
            LockState::Settling {
                head_at_start,
                until,
                ..
            },
            false,
        ) if now >= until => {
            let head_changed = head_at_start.as_ref() != head_now.as_ref();
            *state = if head_changed {
                LockState::Cooldown {
                    until: now + cooldown,
                }
            } else {
                LockState::Idle
            };
            LockTransition::Completed { head_changed }
        }
        (LockState::Cooldown { until }, false) if now >= until => {
            *state = LockState::Idle;
            LockTransition::CooldownEnded
        }
        _ => LockTransition::None,
    }
}

/// `check` fires once per stale period; resets when the lock releases.
#[derive(Debug, Default)]
pub(crate) struct StaleWarn {
    warned: bool,
}

impl StaleWarn {
    pub(crate) fn check(&mut self, state: &LockState, now: Instant) -> Option<Duration> {
        match state {
            // Settling counts as held: `since` spans the merged operation, so
            // a long rebase of short lock cycles still warns (once), and the
            // latch doesn't reset in the sub-second gaps between cycles.
            LockState::Locked { since, .. } | LockState::Settling { since, .. } => {
                let elapsed = now.duration_since(*since);
                if !self.warned && elapsed > Duration::from_secs(STALE_LOCK_SECS) {
                    self.warned = true;
                    return Some(elapsed);
                }
                None
            }
            _ => {
                self.warned = false;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cooldown() -> Duration {
        Duration::from_millis(500)
    }

    #[test]
    fn idle_to_locked_on_lock_appearance() {
        let mut s = LockState::Idle;
        let now = Instant::now();
        assert_eq!(
            drive(&mut s, true, Some("ref: main".into()), now, cooldown()),
            LockTransition::Started
        );
        assert!(matches!(s, LockState::Locked { .. }));
    }

    /// A lock release no longer completes the operation; it opens the settle
    /// window (rapid re-locks merge) and emits nothing.
    #[test]
    fn locked_to_settling_emits_nothing() {
        let now = Instant::now();
        let mut s = LockState::Locked {
            head_at_start: Some("ref: main".into()),
            since: now,
        };
        assert_eq!(
            drive(&mut s, false, Some("ref: feature".into()), now, cooldown()),
            LockTransition::None
        );
        match &s {
            LockState::Settling {
                head_at_start,
                since,
                until,
            } => {
                assert_eq!(head_at_start.as_deref(), Some("ref: main"));
                assert_eq!(*since, now);
                assert_eq!(*until, now + Duration::from_millis(SETTLE_MS));
            }
            other => panic!("expected Settling, got {other:?}"),
        }
    }

    /// Re-lock inside the settle window: the same operation continues, so the
    /// op-start HEAD and `since` are preserved and nothing is emitted (no
    /// duplicate Started — the consumer's in-op flag never flipped).
    #[test]
    fn settling_relock_preserves_op_start_and_emits_nothing() {
        let op_start = Instant::now();
        let later = op_start + Duration::from_millis(100);
        let mut s = LockState::Settling {
            head_at_start: Some("ref: main".into()),
            since: op_start,
            until: later + Duration::from_millis(400),
        };
        assert_eq!(
            drive(&mut s, true, Some("pick-1".into()), later, cooldown()),
            LockTransition::None
        );
        assert_eq!(
            s,
            LockState::Locked {
                head_at_start: Some("ref: main".into()),
                since: op_start,
            },
            "op-start HEAD and since must survive the re-lock"
        );
    }

    /// Settle expiry emits exactly one Completed comparing the first pick's
    /// pre-op HEAD against the final HEAD (head_changed spans the merged op).
    #[test]
    fn settling_expiry_emits_completed_spanning_merged_op() {
        let now = Instant::now();
        let mut s = LockState::Settling {
            head_at_start: Some("ref: main".into()),
            since: now - Duration::from_secs(1),
            until: now,
        };
        assert_eq!(
            drive(&mut s, false, Some("pick-4".into()), now, cooldown()),
            LockTransition::Completed { head_changed: true }
        );
        assert!(matches!(s, LockState::Cooldown { .. }));
    }

    #[test]
    fn settling_expiry_head_unchanged_goes_idle() {
        let now = Instant::now();
        let mut s = LockState::Settling {
            head_at_start: Some("ref: main".into()),
            since: now - Duration::from_secs(1),
            until: now,
        };
        assert_eq!(
            drive(&mut s, false, Some("ref: main".into()), now, cooldown()),
            LockTransition::Completed {
                head_changed: false
            }
        );
        assert_eq!(s, LockState::Idle);
    }

    #[test]
    fn settling_before_expiry_emits_nothing() {
        let now = Instant::now();
        let mut s = LockState::Settling {
            head_at_start: Some("ref: main".into()),
            since: now,
            until: now + Duration::from_millis(1),
        };
        assert_eq!(
            drive(&mut s, false, Some("pick-1".into()), now, cooldown()),
            LockTransition::None
        );
        assert!(matches!(s, LockState::Settling { .. }));
    }

    #[test]
    fn cooldown_to_idle_after_timer() {
        let start = Instant::now();
        let mut s = LockState::Cooldown { until: start };
        let later = start + Duration::from_millis(1);
        assert_eq!(
            drive(&mut s, false, None, later, cooldown()),
            LockTransition::CooldownEnded
        );
        assert_eq!(s, LockState::Idle);
    }

    #[test]
    fn cooldown_to_locked_on_re_acquire() {
        let now = Instant::now();
        let mut s = LockState::Cooldown {
            until: now + Duration::from_millis(500),
        };
        assert_eq!(
            drive(&mut s, true, Some("ref: main".into()), now, cooldown()),
            LockTransition::Started
        );
        assert!(matches!(s, LockState::Locked { .. }));
    }

    /// Regression: timer-arm `drive()` must report Started so the consumer's
    /// `in_op` flag flips; otherwise FilesChanged events skip buffering.
    #[test]
    fn cooldown_to_locked_when_lock_reappears_at_timer_fire() {
        let now = Instant::now();
        let mut s = LockState::Cooldown { until: now };
        assert_eq!(
            drive(&mut s, true, Some("ref: main".into()), now, cooldown()),
            LockTransition::Started,
        );
        assert!(matches!(s, LockState::Locked { .. }));
    }

    #[test]
    fn no_transition_when_idle_and_no_lock() {
        let mut s = LockState::Idle;
        assert_eq!(
            drive(&mut s, false, None, Instant::now(), cooldown()),
            LockTransition::None
        );
        assert_eq!(s, LockState::Idle);
    }

    #[test]
    fn stale_warn_fires_once_per_stale_period() {
        let now = Instant::now();
        let s = LockState::Locked {
            head_at_start: None,
            since: now - Duration::from_secs(STALE_LOCK_SECS + 1),
        };
        let mut w = StaleWarn::default();
        assert!(w.check(&s, now).is_some());
        // Second check while still Locked: latched, no re-fire.
        assert!(w.check(&s, now).is_none());
    }

    #[test]
    fn stale_warn_resets_when_lock_releases() {
        let now = Instant::now();
        let locked = LockState::Locked {
            head_at_start: None,
            since: now - Duration::from_secs(STALE_LOCK_SECS + 1),
        };
        let mut w = StaleWarn::default();
        assert!(w.check(&locked, now).is_some());
        assert!(w.check(&LockState::Idle, now).is_none());
        // Re-acquire: should fire again.
        assert!(w.check(&locked, now).is_some());
    }

    /// A long rebase made of short lock cycles: `since` spans the merged op,
    /// so the warning fires once past the threshold and the settle gaps
    /// between cycles neither reset the latch nor re-fire it.
    #[test]
    fn stale_warn_spans_merged_op_and_stays_latched_through_settling() {
        let now = Instant::now();
        let op_start = now - Duration::from_secs(STALE_LOCK_SECS + 1);
        let settling = LockState::Settling {
            head_at_start: None,
            since: op_start,
            until: now + Duration::from_millis(SETTLE_MS),
        };
        let locked = LockState::Locked {
            head_at_start: None,
            since: op_start,
        };
        let mut w = StaleWarn::default();
        assert!(w.check(&settling, now).is_some(), "settling counts as held");
        assert!(w.check(&locked, now).is_none(), "latched across re-lock");
        assert!(w.check(&settling, now).is_none(), "latched across release");
    }
}
