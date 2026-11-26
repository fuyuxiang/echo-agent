use std::time::Duration;

use echo_agent_sampler::{SamplingErrorInfo, SamplingErrorKind};
use pretty_assertions::assert_eq;

use echo_agent_telemetry::events::RateLimitWaitOutcome;

use super::{
    BudgetLimit, RateLimitWaitBudget, RateLimitWaitConfig, RateLimitWaitDecision,
    RateLimitWaitSummary, WaitOutcome,
};

fn failure(kind: SamplingErrorKind, retry_after_secs: Option<u64>) -> SamplingErrorInfo {
    SamplingErrorInfo {
        kind,
        status_code: matches!(kind, SamplingErrorKind::RateLimited).then_some(429),
        message: "429 concurrent sampling cap exceeded".to_string(),
        is_retryable: false,
        retry_after_secs,
        should_retry: None,
        error_code: None,
        model_metadata: None,
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: echo_agent_sampling_types::SentCredential::Unknown,
    }
}

fn rate_limited(retry_after_secs: Option<u64>) -> SamplingErrorInfo {
    failure(SamplingErrorKind::RateLimited, retry_after_secs)
}

fn config(max_attempts: u32, max_total_wait: Duration) -> RateLimitWaitConfig {
    RateLimitWaitConfig {
        max_attempts,
        max_total_wait,
    }
}

async fn wait_out(decision: RateLimitWaitDecision) -> Duration {
    match decision {
        RateLimitWaitDecision::Wait { backoff, .. } => {
            tokio::time::advance(backoff).await;
            backoff
        }
        other => panic!("expected a wait, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn retry_after_hint_is_capped_and_jittered_like_any_other_wait() {
    let mut budget = RateLimitWaitBudget::for_subagent(RateLimitWaitConfig::default());

    let RateLimitWaitDecision::Wait { attempt, backoff } = budget.decide(&rate_limited(Some(120)))
    else {
        panic!("a subagent 429 within budget must wait");
    };

    assert_eq!(attempt, 1);
    let cap = echo_agent_sampler::MAX_RETRY_BACKOFF;
    assert!(
        backoff >= cap.mul_f32(0.8) && backoff <= cap.mul_f32(1.2),
        "a 120s hint must be capped at {cap:?} and jittered, got {backoff:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cumulative_wait_not_wall_clock_bounds_the_budget() {
    let mut budget = RateLimitWaitBudget::for_subagent(config(8, Duration::from_secs(150)));
    let first = wait_out(budget.decide(&rate_limited(None))).await;
    budget.record_submission_accepted();
    tokio::time::advance(Duration::from_secs(10_000)).await;
    let RateLimitWaitDecision::Wait { attempt, backoff } = budget.decide(&rate_limited(None))
    else {
        panic!("a later 429 must still wait; the turn barely paused");
    };
    assert_eq!(attempt, 2);
    assert!(first + backoff <= Duration::from_secs(150));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_wait_after_recovery_reopens_the_outcome() {
    let mut budget = RateLimitWaitBudget::for_subagent(config(8, Duration::from_secs(600)));
    wait_out(budget.decide(&rate_limited(None))).await;
    budget.record_submission_accepted();
    assert_eq!(budget.summary().unwrap().outcome, WaitOutcome::Recovered);
    wait_out(budget.decide(&rate_limited(None))).await;
    assert_eq!(budget.summary().unwrap().outcome, WaitOutcome::Unresolved);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_wait_past_the_budget_stops_rather_than_truncating() {
    let mut budget = RateLimitWaitBudget::for_subagent(config(8, Duration::from_secs(35)));
    wait_out(budget.decide(&rate_limited(Some(25)))).await;
    assert_eq!(
        budget.decide(&rate_limited(Some(25))),
        RateLimitWaitDecision::BudgetSpent {
            attempts: 1,
            limit: BudgetLimit::TotalWait,
        }
    );
}

#[test]
fn main_sessions_disabled_configs_and_non_429_failures_never_wait() {
    let mut main_session = RateLimitWaitBudget::for_main_session();
    assert_eq!(
        main_session.decide(&rate_limited(Some(1))),
        RateLimitWaitDecision::Disabled
    );

    let mut disabled = RateLimitWaitBudget::for_subagent(config(0, Duration::from_secs(150)));
    assert_eq!(
        disabled.decide(&rate_limited(Some(1))),
        RateLimitWaitDecision::Disabled
    );
    assert!(
        !disabled.can_wait(),
        "no request clone is needed when waiting is off"
    );

    let mut subagent = RateLimitWaitBudget::for_subagent(config(2, Duration::from_secs(600)));
    assert_eq!(
        subagent.decide(&failure(SamplingErrorKind::Api, None)),
        RateLimitWaitDecision::NotRateLimited
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn running_out_of_attempts_stops_the_waiting_and_names_that_cause() {
    let mut budget = RateLimitWaitBudget::for_subagent(config(2, Duration::from_secs(600)));

    for _ in 0..2 {
        let decision = budget.decide(&rate_limited(None));
        wait_out(decision).await;
    }

    let spent = budget.decide(&rate_limited(None));
    assert_eq!(
        spent,
        RateLimitWaitDecision::BudgetSpent {
            attempts: 2,
            limit: BudgetLimit::Attempts,
        }
    );
    assert_eq!(budget.summary().unwrap().outcome, WaitOutcome::BudgetSpent);
}

#[test]
fn budget_limit_cause_strings_are_stable() {
    assert_eq!(BudgetLimit::Attempts.as_str(), "attempts_spent");
    assert_eq!(BudgetLimit::TotalWait.as_str(), "deadline_spent");
}

#[test]
fn default_attempts_ladder_exhausts_exactly_at_the_budget() {
    let cap_ms = echo_agent_sampler::MAX_RETRY_BACKOFF.as_millis() as u64;
    // Mirrors `retry_backoff_with_jitter`'s pre-jitter base (2s doubling, capped);
    // the 2s base is pinned by the sampler's own backoff test.
    let ladder: Duration = (1..=RateLimitWaitConfig::DEFAULT_MAX_ATTEMPTS)
        .map(|attempt| {
            let base_ms = 2000u64
                .checked_shl(attempt - 1)
                .unwrap_or(u64::MAX)
                .min(cap_ms);
            Duration::from_millis(base_ms)
        })
        .sum();
    assert_eq!(
        ladder,
        RateLimitWaitConfig::DEFAULT_MAX_TOTAL_WAIT,
        "attempts default, backoff ladder, and cumulative-wait budget must move together"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn summary_reports_waits_and_the_recovered_or_unresolved_outcome() {
    let mut budget = RateLimitWaitBudget::for_subagent(config(4, Duration::from_secs(600)));
    assert_eq!(budget.summary(), None);

    let mut waited = Duration::ZERO;
    for _ in 0..2 {
        waited += wait_out(budget.decide(&rate_limited(None))).await;
    }
    assert_eq!(
        budget.summary(),
        Some(RateLimitWaitSummary {
            attempts: 2,
            total_waited: waited,
            outcome: WaitOutcome::Unresolved,
        })
    );

    budget.record_submission_accepted();
    assert_eq!(budget.summary().unwrap().outcome, WaitOutcome::Recovered);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn drop_reports_a_telemetry_row_only_when_the_turn_waited() {
    let quiet = RateLimitWaitBudget::for_subagent(config(4, Duration::from_secs(600)));
    assert!(quiet.telemetry_event().is_none());

    let mut budget = RateLimitWaitBudget::for_subagent(config(4, Duration::from_secs(600)));
    for _ in 0..2 {
        wait_out(budget.decide(&rate_limited(None))).await;
    }
    budget.record_submission_accepted();

    let event = budget
        .telemetry_event()
        .expect("a turn that waited must report one row");
    assert_eq!(event.attempts, 2);
    assert_eq!(event.max_attempts, 4);
    assert_eq!(event.outcome, RateLimitWaitOutcome::Recovered);
}
