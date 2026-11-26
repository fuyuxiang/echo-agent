//! Telemetry hook trait for [`crate::CircuitBreaker`].
//!
//! Observer methods are invoked **outside** the breaker's internal
//! locks and **after** any state transition is visible via
//! [`crate::CircuitBreaker::state`]. Observer impls must not block or
//! perform unbounded I/O — they sit on every `record()` / `check()`
//! hot path. Short non-contended locks (e.g. a Prometheus per-label
//! mutex) are fine.

use crate::state::{BreakerState, Outcome};

/// Telemetry hooks. Default implementations are no-ops; consumers
/// implement only the methods they care about.
pub trait Observer: Send + Sync {
    /// Called when the breaker transitions between states. `reason`
    /// is a short stable string (e.g. `"trip"`, `"probe_success"`,
    /// `"probe_failure"`, `"open_elapsed"`).
    fn on_state_change(&self, _old: BreakerState, _new: BreakerState, _reason: &str) {}

    /// Called from `check()` when the breaker is `HalfOpen` and a
    /// caller attempts to claim a probe slot. `allowed = false` means
    /// `half_open_max_probes` was already in flight.
    fn on_probe_admission(&self, _allowed: bool) {}

    /// Called from `record()` after the sample is added to the window
    /// and any resulting state transition has landed. `status` is the
    /// post-transition state.
    fn on_outcome(&self, _outcome: Outcome, _status: BreakerState) {}
}

/// No-op observer used by [`crate::CircuitBreaker::new`].
#[derive(Debug, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {}
