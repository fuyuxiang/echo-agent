//! Per-turn 429 waiting for subagent submissions; main sessions never wait.

use std::time::Duration;

use echo_agent_sampler::{SamplingErrorInfo, SamplingErrorKind};
use echo_agent_telemetry::events::{
    RateLimitWaitOutcome as ReportedOutcome, SubagentRateLimitWaited,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimitWaitConfig {
    pub(crate) max_attempts: u32,
    pub(crate) max_total_wait: Duration,
}

impl Default for RateLimitWaitConfig {
    fn default() -> Self {
        Self {
            max_attempts: Self::DEFAULT_MAX_ATTEMPTS,
            max_total_wait: Self::DEFAULT_MAX_TOTAL_WAIT,
        }
    }
}

impl RateLimitWaitConfig {
    /// Default subagent 429 wait attempts; `0` disables waiting.
    pub(crate) const DEFAULT_MAX_ATTEMPTS: u32 = 8;
    /// Hard cap on a configured value.
    pub(crate) const MAX_ATTEMPTS_CAP: u32 = 32;
    /// Per-turn cumulative-wait budget (sum of backoffs), coupled to
    /// [`Self::DEFAULT_MAX_ATTEMPTS`] so both exhaust together (see the coupling
    /// test); not a user knob.
    pub(crate) const DEFAULT_MAX_TOTAL_WAIT: Duration = Duration::from_secs(150);

    /// Resolved attempts (clamped to the cap) with the fixed default budget.
    pub(crate) fn with_max_attempts(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.min(Self::MAX_ATTEMPTS_CAP),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitWaitDecision {
    /// `attempt` is 1-indexed within the turn.
    Wait {
        attempt: u32,
        backoff: Duration,
    },
    Disabled,
    NotRateLimited,
    BudgetSpent {
        attempts: u32,
        limit: BudgetLimit,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetLimit {
    Attempts,
    TotalWait,
}

impl BudgetLimit {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Attempts => "attempts_spent",
            Self::TotalWait => "deadline_spent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RateLimitWaitSummary {
    attempts: u32,
    total_waited: Duration,
    outcome: WaitOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Recovered,
    BudgetSpent,
    /// The turn ended mid-wait: cancelled, or failed for another reason.
    Unresolved,
}

/// One `process_conversation_turn`'s rate-limit budget, shared across that
/// turn's model round-trips. Bounds cumulative pause time, not wall-clock.
pub(crate) struct RateLimitWaitBudget {
    state: Option<BudgetState>,
}

struct BudgetState {
    config: RateLimitWaitConfig,
    attempts: u32,
    total_waited: Duration,
    outcome: WaitOutcome,
}

impl RateLimitWaitBudget {
    fn for_main_session() -> Self {
        Self { state: None }
    }

    fn for_subagent(config: RateLimitWaitConfig) -> Self {
        Self {
            state: (config.max_attempts > 0).then_some(BudgetState {
                config,
                attempts: 0,
                total_waited: Duration::ZERO,
                outcome: WaitOutcome::Unresolved,
            }),
        }
    }

    pub(crate) fn can_wait(&self) -> bool {
        self.state.is_some()
    }

    pub(crate) fn attempts_used(&self) -> u32 {
        self.state.as_ref().map_or(0, |state| state.attempts)
    }

    pub(crate) fn max_attempts(&self) -> u32 {
        self.state
            .as_ref()
            .map_or(0, |state| state.config.max_attempts)
    }

    pub(crate) fn decide(&mut self, error: &SamplingErrorInfo) -> RateLimitWaitDecision {
        let Some(state) = self.state.as_mut() else {
            return RateLimitWaitDecision::Disabled;
        };
        if !matches!(error.kind, SamplingErrorKind::RateLimited) {
            return RateLimitWaitDecision::NotRateLimited;
        }
        state.decide_rate_limited(error.retry_after_secs)
    }

    pub(crate) fn record_submission_accepted(&mut self) {
        if let Some(state) = self.state.as_mut()
            && state.attempts > 0
        {
            state.outcome = WaitOutcome::Recovered;
        }
    }

    fn summary(&self) -> Option<RateLimitWaitSummary> {
        let state = self.state.as_ref().filter(|state| state.attempts > 0)?;
        Some(RateLimitWaitSummary {
            attempts: state.attempts,
            total_waited: state.total_waited,
            outcome: state.outcome,
        })
    }

    /// The telemetry row for this turn's waiting, or `None` when it never waited.
    fn telemetry_event(&self) -> Option<SubagentRateLimitWaited> {
        let summary = self.summary()?;
        let config = self.state.as_ref().map(|s| s.config)?;
        Some(SubagentRateLimitWaited {
            attempts: summary.attempts,
            max_attempts: config.max_attempts,
            // Sum of planned backoffs; on cancel (`Unresolved`) mid-wait this
            // can overstate wall-clock by up to one backoff.
            waited_ms: summary.total_waited.as_millis() as u64,
            budget_ms: config.max_total_wait.as_millis() as u64,
            outcome: match summary.outcome {
                WaitOutcome::Recovered => ReportedOutcome::Recovered,
                WaitOutcome::BudgetSpent => ReportedOutcome::BudgetSpent,
                WaitOutcome::Unresolved => ReportedOutcome::Unresolved,
            },
        })
    }
}

impl super::SessionActor {
    pub(crate) fn rate_limit_wait_budget(&self) -> RateLimitWaitBudget {
        if self.startup_hints.is_subagent {
            RateLimitWaitBudget::for_subagent(self.rate_limit_waits)
        } else {
            RateLimitWaitBudget::for_main_session()
        }
    }
}

impl BudgetState {
    fn decide_rate_limited(&mut self, retry_after_secs: Option<u64>) -> RateLimitWaitDecision {
        if self.attempts >= self.config.max_attempts {
            self.outcome = WaitOutcome::BudgetSpent;
            return RateLimitWaitDecision::BudgetSpent {
                attempts: self.attempts,
                limit: BudgetLimit::Attempts,
            };
        }
        let attempt = self.attempts + 1;
        let wait = echo_agent_sampler::retry_after_or_backoff(attempt, retry_after_secs);
        // An over-budget wait stops rather than truncating, which would
        // resubmit before the server's window clears.
        if self.total_waited + wait > self.config.max_total_wait {
            self.outcome = WaitOutcome::BudgetSpent;
            return RateLimitWaitDecision::BudgetSpent {
                attempts: self.attempts,
                limit: BudgetLimit::TotalWait,
            };
        }
        self.attempts = attempt;
        self.total_waited += wait;
        // A fresh wait re-opens the turn: a submit accepted earlier flipped the
        // outcome to Recovered, but a cancel mid-this-wait is Unresolved, not
        // Recovered.
        self.outcome = WaitOutcome::Unresolved;
        RateLimitWaitDecision::Wait {
            attempt,
            backoff: wait,
        }
    }
}

/// Reported from `Drop` so a cancel (task abort) still records its waits.
impl Drop for RateLimitWaitBudget {
    fn drop(&mut self) {
        if let Some(event) = self.telemetry_event() {
            echo_agent_telemetry::session_ctx::log_event(event);
        }
    }
}

#[cfg(test)]
#[path = "rate_limit_waits_tests.rs"]
mod tests;
