use std::time::Duration;

use echo_agent_sampling_types::SentCredential;

use super::{AuthRetryDecision, AuthRetrySchedule};
use crate::util::dual_clock::DualClock;

/// `now` shifted `wall_ahead` on the wall clock only — the signature a
/// suspend leaves behind (monotonic pauses, wall keeps advancing).
fn after_suspend(base: DualClock, wall_ahead: Duration) -> DualClock {
    DualClock {
        mono: base.mono,
        wall: base.wall + wall_ahead,
    }
}

/// Pins the exact schedule. Guards against the `from_millis(1000)` footgun
/// (baseⁿ semantics), which produced field sleeps of 1s, 16m40s, and 11.57
/// days.
#[test]
fn schedule_is_one_two_four_seconds_then_exhausted() {
    let mut schedule = AuthRetrySchedule::new();
    let steps: Vec<_> = (0..3)
        .map(|_| schedule.on_recovered_401(SentCredential::Sent))
        .collect();
    assert_eq!(
        steps,
        vec![
            AuthRetryDecision::Backoff {
                attempt: 1,
                delay: Duration::from_secs(1)
            },
            AuthRetryDecision::Backoff {
                attempt: 2,
                delay: Duration::from_secs(2)
            },
            AuthRetryDecision::Backoff {
                attempt: 3,
                delay: Duration::from_secs(4)
            },
        ],
    );
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Sent),
        AuthRetryDecision::Exhausted,
    );
    assert_eq!(schedule.incident_counts(), (4, 4));
}

/// Unknown provenance charges like an authenticated 401 (fail closed toward
/// terminating) but is not reported as a proven credential rejection.
#[test]
fn unknown_credential_charges_but_is_not_counted_authenticated() {
    let mut schedule = AuthRetrySchedule::new();
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Unknown),
        AuthRetryDecision::Backoff {
            attempt: 1,
            delay: Duration::from_secs(1)
        },
    );
    assert_eq!(schedule.incident_counts(), (1, 0));
}

/// The overnight-failure regression: a credential-less 401 never consumes a
/// budget slot; only the runaway guard bounds it.
#[test]
fn missing_credential_never_charges_until_runaway_guard() {
    let mut schedule = AuthRetrySchedule::new();
    for i in 1..=AuthRetrySchedule::MAX_UNCHARGED_RESUBMITS {
        assert_eq!(
            schedule.on_recovered_401(SentCredential::Missing),
            AuthRetryDecision::UnchargedResubmit { resubmit: i },
        );
    }
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Missing),
        AuthRetryDecision::RunawayGuard {
            rejections: AuthRetrySchedule::MAX_UNCHARGED_RESUBMITS + 1
        },
    );
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Sent),
        AuthRetryDecision::Backoff {
            attempt: 1,
            delay: Duration::from_secs(1)
        },
        "the credentialed budget must be untouched throughout"
    );
}

/// A success ends every open failure narrative: the escalating delays, the
/// attempt numbering, and the runaway counter all restart (a 200 disproves
/// the runaway premise, so a productive multi-day turn can never accumulate
/// into the guard).
#[test]
fn success_resets_budget_and_uncharged_counter() {
    let mut schedule = AuthRetrySchedule::new();
    schedule.on_recovered_401(SentCredential::Sent);
    schedule.on_recovered_401(SentCredential::Sent);
    for _ in 0..AuthRetrySchedule::MAX_UNCHARGED_RESUBMITS {
        schedule.on_recovered_401(SentCredential::Missing);
    }
    schedule.reset_on_success();
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Missing),
        AuthRetryDecision::UnchargedResubmit { resubmit: 1 },
    );
    assert_eq!(
        schedule.on_recovered_401(SentCredential::Sent),
        AuthRetryDecision::Backoff {
            attempt: 1,
            delay: Duration::from_secs(1)
        },
    );
}

/// The uncharged counter survives a suspend reset (the guard spans sleep
/// cycles — that is its point) while the charged budget restarts.
#[test]
fn suspend_reset_preserves_uncharged_counter() {
    let mut schedule = AuthRetrySchedule::new();
    let start = DualClock::now();
    schedule.on_recovered_401_at(SentCredential::Missing, start);
    schedule.on_recovered_401_at(SentCredential::Missing, start);
    schedule.on_recovered_401_at(SentCredential::Sent, start);

    let woke = after_suspend(start, Duration::from_secs(16 * 60));
    assert!(schedule.reset_if_incident_spans_suspend_at(woke));
    assert_eq!(
        schedule.on_recovered_401_at(SentCredential::Missing, woke),
        AuthRetryDecision::UnchargedResubmit { resubmit: 3 },
    );
    assert_eq!(
        schedule.on_recovered_401_at(SentCredential::Sent, woke),
        AuthRetryDecision::Backoff {
            attempt: 1,
            delay: Duration::from_secs(1)
        },
        "post-suspend 401 starts a fresh incident instead of exhausting"
    );
}

/// Suspend resets are capped per success-free stretch: a fault that
/// persists across wakes must eventually exhaust instead of retrying
/// forever. A success re-arms the cap.
#[test]
fn suspend_resets_cap_without_success_and_rearm_on_success() {
    let mut schedule = AuthRetrySchedule::new();
    let mut now = DualClock::now();
    for _ in 0..AuthRetrySchedule::MAX_SUSPEND_RESETS {
        schedule.on_recovered_401_at(SentCredential::Sent, now);
        now = after_suspend(now, Duration::from_secs(16 * 60));
        assert!(schedule.reset_if_incident_spans_suspend_at(now));
    }
    schedule.on_recovered_401_at(SentCredential::Sent, now);
    now = after_suspend(now, Duration::from_secs(16 * 60));
    assert!(
        !schedule.reset_if_incident_spans_suspend_at(now),
        "reset {} must be refused: the budget is now allowed to exhaust",
        AuthRetrySchedule::MAX_SUSPEND_RESETS + 1
    );

    schedule.reset_on_success();
    schedule.on_recovered_401_at(SentCredential::Sent, now);
    now = after_suspend(now, Duration::from_secs(16 * 60));
    assert!(
        schedule.reset_if_incident_spans_suspend_at(now),
        "a success re-arms the suspend-reset cap"
    );
}

/// No suspend, no reset: sub-threshold wall drift (NTP jitter) and a
/// schedule with no open incident are both no-ops.
#[test]
fn suspend_reset_requires_open_incident_and_real_drift() {
    let mut schedule = AuthRetrySchedule::new();
    let start = DualClock::now();
    assert!(
        !schedule
            .reset_if_incident_spans_suspend_at(after_suspend(start, Duration::from_secs(3600))),
        "no open incident: nothing to reset"
    );
    schedule.on_recovered_401_at(SentCredential::Sent, start);
    assert!(
        !schedule.reset_if_incident_spans_suspend_at(after_suspend(start, Duration::from_secs(5))),
        "5s wall drift is NTP-jitter territory, not a suspend"
    );
    assert_eq!(
        schedule.on_recovered_401_at(SentCredential::Sent, start),
        AuthRetryDecision::Backoff {
            attempt: 2,
            delay: Duration::from_secs(2)
        },
        "the failed reset checks must not charge the budget"
    );
}
