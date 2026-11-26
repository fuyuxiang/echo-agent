//! User-cancel latency measurement.
//!
//! The small data types plus the settle rule behind the `CancellationCompleted`
//! telemetry event. All consumers live in `agent_view` (arm/settle) and
//! `dispatch` (the cancel call sites); nothing in `agent.rs` uses them.
use echo_agent_telemetry::events::CancellationScope;
use std::time::Instant;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CancelOrigin {
    UserGesture,
    #[allow(dead_code)]
    Programmatic,
}
/// How a turn ended, which decides whether a pending user-cancel anchor is measured.
/// `Completed` = the turn reached its own terminal outcome (finished, or an honored cancel settled) so the cancel-latency anchor is measured and emitted; `Aborted` = the view was force-idled by reload/fork/session-failure, so the anchor is discarded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TurnEnd {
    Completed,
    Aborted,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct CancelLatency {
    pub(crate) requested_at: Instant,
    pub(crate) scope: CancellationScope,
}
impl CancelLatency {
    pub(crate) fn new(requested_at: Instant, scope: CancellationScope) -> Self {
        Self {
            requested_at,
            scope,
        }
    }
}
