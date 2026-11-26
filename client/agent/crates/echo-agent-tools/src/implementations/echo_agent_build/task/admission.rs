//! Session-scoped subagent spawn limits, enforced by the coordinator.

use super::types::SubagentRequest;
use crate::util::env::parse_positive_env;

pub const DEFAULT_MAX_CONCURRENT: usize = 32;

/// What happens to a spawn that arrives at the concurrent limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LimitBehavior {
    #[default]
    Queue,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentLimits {
    /// Subagents one session may run at once. Zero does not disable the
    /// limit: [`Admission::new`] clamps it to 1.
    pub max_concurrent: usize,
    pub behavior: LimitBehavior,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            behavior: LimitBehavior::Queue,
        }
    }
}

impl SubagentLimits {
    /// Read once at the composition root; inject everywhere else.
    pub fn from_env() -> Self {
        Self::from_lookup(|var| std::env::var(var).ok())
    }

    /// A limit can be adjusted but never disabled.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let default = Self::default();
        let max_concurrent = parse_positive_env(
            "ECHO_AGENT_MAX_CONCURRENT_SUBAGENTS",
            lookup("ECHO_AGENT_MAX_CONCURRENT_SUBAGENTS"),
        )
        .unwrap_or(default.max_concurrent);
        let behavior = match lookup("ECHO_AGENT_SUBAGENT_LIMIT_BEHAVIOR") {
            None => LimitBehavior::Queue,
            Some(value) if value.eq_ignore_ascii_case("fail") => LimitBehavior::Fail,
            Some(value) if value.eq_ignore_ascii_case("queue") => LimitBehavior::Queue,
            Some(value) => {
                tracing::warn!(
                    %value,
                    "ECHO_AGENT_SUBAGENT_LIMIT_BEHAVIOR is neither `queue` nor `fail`; keeping `queue`"
                );
                LimitBehavior::Queue
            }
        };
        Self {
            max_concurrent,
            behavior,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdmissionDecision {
    Start,
    Enqueue,
    Reject(AdmissionError),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdmissionError {
    ConcurrentLimitReached { limit: usize },
}

impl AdmissionError {
    /// Model-facing failure text; reachable only when an operator opts
    /// into `ECHO_AGENT_SUBAGENT_LIMIT_BEHAVIOR=fail`.
    pub(super) fn message(&self) -> String {
        match self {
            Self::ConcurrentLimitReached { limit } => format!(
                "Concurrent subagent limit reached: {limit} subagents are already running for \
                 this session. Do not retry; spawning succeeds again when a running subagent \
                 finishes."
            ),
        }
    }
}

/// Admission policy; the coordinator owns the state it judges against.
pub(super) struct Admission {
    limits: SubagentLimits,
}

impl Admission {
    /// `max_concurrent: 0` is clamped to 1 (the env path already filters
    /// zero): a limit can be adjusted but never disabled, and the actor's
    /// "queued implies running" exit invariant needs at least one slot.
    pub(super) fn new(mut limits: SubagentLimits) -> Self {
        limits.max_concurrent = limits.max_concurrent.max(1);
        Self { limits }
    }

    /// `running` is the session's live non-workflow child count.
    pub(super) fn admit(&self, request: &SubagentRequest, running: usize) -> AdmissionDecision {
        if request.owner.is_workflow() {
            // Workflow agents follow the run's own pool.
            return AdmissionDecision::Start;
        }
        if self.has_capacity(running) {
            AdmissionDecision::Start
        } else {
            match self.limits.behavior {
                LimitBehavior::Queue => AdmissionDecision::Enqueue,
                LimitBehavior::Fail => {
                    AdmissionDecision::Reject(AdmissionError::ConcurrentLimitReached {
                        limit: self.limits.max_concurrent,
                    })
                }
            }
        }
    }

    pub(super) fn has_capacity(&self, running: usize) -> bool {
        running < self.limits.max_concurrent
    }

    /// The one configured value; the coordinator must not re-read its config.
    pub(super) fn max_concurrent(&self) -> usize {
        self.limits.max_concurrent
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
