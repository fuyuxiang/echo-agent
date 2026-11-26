//! Tracing-based observer for the storage circuit breaker.

use echo_agent_circuit_breaker::{BreakerState, Observer, Outcome};
use std::sync::Arc;

/// `Observer` impl that emits `tracing` events matching the legacy
/// in-tree `circuit_breaker.rs` so existing analytics queries
/// (`target=circuit_breaker AND breaker=storage_breaker`) keep firing.
///
/// Event routing keys on the **new** state — keying on `(old, new)`
/// tuples invites arm-ordering bugs (an early `(Open, _)` arm would
/// catch `Open -> HalfOpen` and mis-label it "closed").
///
/// | new state | level | message                  |
/// |-----------|-------|--------------------------|
/// | `Open`    | warn  | "circuit breaker opened" |
/// | `HalfOpen`| debug | "circuit breaker half-open" |
/// | `Closed`  | info  | "circuit breaker closed" |
///
/// `on_outcome` emits a `tracing::trace!` per `Outcome::Failure` so
/// downstream failure-rate dashboards have a per-401 signal.
/// Successes are dropped (steady state would otherwise dominate log volume).
pub(crate) struct TracingObserver {
    name: &'static str,
}

impl TracingObserver {
    pub(crate) fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name })
    }
}

impl Observer for TracingObserver {
    fn on_state_change(&self, old: BreakerState, new: BreakerState, reason: &str) {
        match new {
            BreakerState::Open => tracing::warn!(
                target: "circuit_breaker",
                breaker = self.name,
                ?old,
                ?new,
                reason,
                "circuit breaker opened"
            ),
            BreakerState::HalfOpen => tracing::debug!(
                target: "circuit_breaker",
                breaker = self.name,
                ?old,
                ?new,
                reason,
                "circuit breaker half-open"
            ),
            BreakerState::Closed => tracing::info!(
                target: "circuit_breaker",
                breaker = self.name,
                ?old,
                ?new,
                reason,
                "circuit breaker closed"
            ),
        }
    }

    fn on_probe_admission(&self, allowed: bool) {
        tracing::debug!(
            target: "circuit_breaker",
            breaker = self.name,
            allowed,
            "circuit breaker probe admission"
        );
    }

    fn on_outcome(&self, outcome: Outcome, state: BreakerState) {
        if let Outcome::Failure = outcome {
            tracing::trace!(
                target: "circuit_breaker",
                breaker = self.name,
                ?state,
                "circuit breaker outcome failure"
            );
        }
    }
}

#[cfg(test)]
#[path = "circuit_breaker_observer_tests.rs"]
mod tests;
