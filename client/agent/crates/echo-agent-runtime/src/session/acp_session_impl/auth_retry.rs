//! Per-turn retry policy for 401s that follow a *successful* auth recovery
//! (fresh token minted, request to be re-sent).

use echo_agent_sampling_types::SentCredential;
use tokio_retry::strategy::ExponentialBackoff;

use super::RecoveredStore;
use crate::auth::AuthManager;
use crate::util::dual_clock::DualClock;

/// Pace an uncharged resubmit: wait (bounded) for a session-token refresh
/// when that is the store the recovery minted into and nothing wire-valid
/// has landed yet; otherwise floor-pace so the runaway guard can never be a
/// burst of back-to-back requests. Auth policy lives here, not in the turn
/// loop.
pub(crate) async fn pace_uncharged_resubmit(
    store: RecoveredStore,
    auth_manager: Option<&std::sync::Arc<AuthManager>>,
) {
    match (store, auth_manager) {
        (RecoveredStore::SessionToken, Some(am)) if am.current_wire_valid().is_none() => {
            am.wait_for_token_refresh(AuthRetrySchedule::UNCHARGED_REFRESH_WAIT)
                .await;
        }
        _ => tokio::time::sleep(AuthRetrySchedule::UNCHARGED_RESUBMIT_FLOOR).await,
    }
}

/// Compact `2h3m` / `4m7s` / `12s` rendering for turn-failure messages.
pub(crate) fn human_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs < 60 {
        return format!("{total_secs}s");
    }
    let mins = total_secs / 60;
    if mins < 60 {
        return format!("{mins}m{}s", total_secs % 60);
    }
    format!("{}h{}m", mins / 60, mins % 60)
}

/// Decision for one post-recovery 401 (see
/// [`AuthRetrySchedule::on_recovered_401`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthRetryDecision {
    /// No credential was on the wire, so no slot is charged; resubmit after
    /// the refresh lands. `resubmit` is the 1-indexed count since the last
    /// successful response.
    UnchargedResubmit { resubmit: u32 },
    /// Charged one escalating slot: back off `delay`, then resubmit.
    Backoff {
        attempt: u32,
        delay: std::time::Duration,
    },
    /// Per-incident budget exhausted by credentialed 401s — fail the turn.
    Exhausted,
    /// Runaway guard tripped: recovery kept succeeding while the server
    /// rejected `rejections` credential-less requests without a single
    /// successful response — fail the turn.
    RunawayGuard { rejections: u32 },
}

/// Escalating retry budget for post-recovery 401s. The budget is
/// per-incident (successes and suspend boundaries reset it, the latter
/// capped) and only credentialed rejections charge it; the doc on each
/// method carries its own invariant. One thing no reader can derive from
/// the code:
///
/// **Delays must be 1s/2s/4s.** `ExponentialBackoff::from_millis(base)`
/// raises `base` to the attempt number, so the base must stay small:
/// `from_millis(1000)` yields 1000ⁿ ms = 1s → 16m40s → 11.57 days of
/// silent hang (a past field incident). `from_millis(2).factor(500)`
/// yields 1s, 2s, 4s.
pub(crate) struct AuthRetrySchedule {
    delays: std::iter::Take<ExponentialBackoff>,
    /// Slots charged this incident.
    attempt: u32,
    /// 401s seen this incident, total and the subset that provably carried
    /// a credential. Feeds the exhaustion message so "real credential
    /// rejected" and "budget exhausted" cannot be conflated.
    incident_rejections: u32,
    incident_authenticated: u32,
    /// Stamped by the incident's first charged 401; cleared by resets.
    incident_started: Option<DualClock>,
    /// Uncharged fail-closed rejections since the last successful response
    /// (survives suspend resets).
    uncharged_resubmits: u32,
    /// Suspend-triggered resets since the last successful response.
    suspend_resets: u32,
}

impl AuthRetrySchedule {
    /// Consecutive credentialed post-recovery 401s tolerated per incident
    /// before the turn fails.
    pub(crate) const MAX_RETRIES: u32 = 3;
    /// Runaway guard: uncharged (no-credential) rejections tolerated
    /// without an intervening successful response (~one per 16-minute
    /// sleep cycle ⇒ >13 h lid-closed survival).
    pub(crate) const MAX_UNCHARGED_RESUBMITS: u32 = 50;
    /// Suspend resets tolerated without an intervening successful response
    /// (~8 sleep cycles of a continuously failing incident) before the
    /// budget stops resetting and is allowed to exhaust.
    pub(crate) const MAX_SUSPEND_RESETS: u32 = 8;
    /// Bounded wait for the proactive refresh / wake nudge to land a
    /// wire-valid token before an uncharged resubmit.
    const UNCHARGED_REFRESH_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
    /// Floor pacing for uncharged resubmits with no refresh to wait on.
    const UNCHARGED_RESUBMIT_FLOOR: std::time::Duration = std::time::Duration::from_secs(1);
    /// Wall-vs-monotonic drift beyond which the machine must have slept:
    /// well below a real sleep cycle (minutes), well above NTP step jitter.
    const SUSPEND_DRIFT_MIN: std::time::Duration = std::time::Duration::from_secs(30);

    pub(crate) fn new() -> Self {
        Self {
            delays: ExponentialBackoff::from_millis(2)
                .factor(500)
                .max_delay(std::time::Duration::from_secs(10))
                .take(Self::MAX_RETRIES as usize),
            attempt: 0,
            incident_rejections: 0,
            incident_authenticated: 0,
            incident_started: None,
            uncharged_resubmits: 0,
            suspend_resets: 0,
        }
    }

    /// Decision for one post-recovery 401. Charges a slot only when the
    /// rejected request carried a credential (or its provenance is unknown
    /// — fail closed toward terminating).
    pub(crate) fn on_recovered_401(&mut self, credential: SentCredential) -> AuthRetryDecision {
        self.on_recovered_401_at(credential, DualClock::now())
    }

    /// Clock-injected twin of [`Self::on_recovered_401`] for tests.
    fn on_recovered_401_at(
        &mut self,
        credential: SentCredential,
        now: DualClock,
    ) -> AuthRetryDecision {
        if credential.is_missing() {
            self.uncharged_resubmits += 1;
            if self.uncharged_resubmits > Self::MAX_UNCHARGED_RESUBMITS {
                return AuthRetryDecision::RunawayGuard {
                    rejections: self.uncharged_resubmits,
                };
            }
            return AuthRetryDecision::UnchargedResubmit {
                resubmit: self.uncharged_resubmits,
            };
        }
        self.incident_started.get_or_insert(now);
        self.incident_rejections += 1;
        if credential == SentCredential::Sent {
            self.incident_authenticated += 1;
        }
        match self.delays.next() {
            Some(delay) => {
                self.attempt += 1;
                AuthRetryDecision::Backoff {
                    attempt: self.attempt,
                    delay,
                }
            }
            None => AuthRetryDecision::Exhausted,
        }
    }

    /// Close the open incident if it spans a suspend (wall elapsed outgrew
    /// monotonic elapsed by [`Self::SUSPEND_DRIFT_MIN`]): separate wakes are
    /// independent 401 events. Capped at [`Self::MAX_SUSPEND_RESETS`] per
    /// success-free stretch so a fault that persists across wakes exhausts
    /// instead of retrying forever. Returns whether a reset happened.
    pub(crate) fn reset_if_incident_spans_suspend(&mut self) -> bool {
        self.reset_if_incident_spans_suspend_at(DualClock::now())
    }

    /// Clock-injected twin of [`Self::reset_if_incident_spans_suspend`].
    fn reset_if_incident_spans_suspend_at(&mut self, now: DualClock) -> bool {
        let Some(started) = self.incident_started else {
            return false;
        };
        if self.suspend_resets >= Self::MAX_SUSPEND_RESETS {
            return false;
        }
        let (awake, total) = started.elapsed_between(now);
        if total.saturating_sub(awake) < Self::SUSPEND_DRIFT_MIN {
            return false;
        }
        let (uncharged, resets) = (self.uncharged_resubmits, self.suspend_resets);
        *self = Self::new();
        self.uncharged_resubmits = uncharged;
        self.suspend_resets = resets + 1;
        true
    }

    /// A successful model response ends every open failure narrative:
    /// restart the escalating schedule and clear the success-free-stretch
    /// counters (uncharged rejections, suspend resets).
    pub(crate) fn reset_on_success(&mut self) {
        *self = Self::new();
    }

    /// `(rejections, authenticated)` seen this incident, for the exhaustion
    /// message.
    pub(crate) fn incident_counts(&self) -> (u32, u32) {
        (self.incident_rejections, self.incident_authenticated)
    }

    /// Uncharged fail-closed rejections since the last successful response.
    pub(crate) fn uncharged_rejections(&self) -> u32 {
        self.uncharged_resubmits
    }
}

#[cfg(test)]
#[path = "auth_retry_tests.rs"]
mod tests;
