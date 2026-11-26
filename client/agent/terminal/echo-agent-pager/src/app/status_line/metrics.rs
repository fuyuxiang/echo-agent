//! Status row telemetry: adoption for every session, health for the sessions
//! that drew a row.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use echo_agent_status_line::{
    ResolvedStatusLine, StatusLineConfig, StatusLineItem, StatusLineType,
};

use super::draws_a_row;

/// Process-wide counters, since a run records from a detached task and both
/// health-reporting paths run with no view in scope. One row per process:
/// `kind` latches once.
pub(crate) fn global() -> &'static StatusLineMetrics {
    static METRICS: StatusLineMetrics = StatusLineMetrics::new();
    &METRICS
}

#[derive(Debug)]
pub(crate) struct StatusLineMetrics {
    /// Latched so a second caller cannot report the session twice.
    kind: OnceLock<&'static str>,
    draws_a_row: AtomicBool,
    had_content: AtomicBool,
    reported: AtomicBool,
    ok: AtomicU64,
    failed: AtomicU64,
    timed_out: AtomicU64,
    abandoned: AtomicU64,
    slowest_ms: AtomicU64,
}

impl StatusLineMetrics {
    const fn new() -> Self {
        Self {
            kind: OnceLock::new(),
            draws_a_row: AtomicBool::new(false),
            had_content: AtomicBool::new(false),
            reported: AtomicBool::new(false),
            ok: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
            abandoned: AtomicU64::new(0),
            slowest_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn note_content(&self) {
        self.had_content.store(true, Ordering::Relaxed);
    }

    /// Called once per session at the config boundary, row or not: the
    /// sessions that never touched the feature are adoption's denominator.
    pub(crate) fn report_config(&self, cfg: &StatusLineConfig) {
        // `unset`, not `disabled`: no readable mode, whether the section named none
        // or its `type` was rejected. `row_shows_a_problem` separates those.
        let kind = cfg.declared_kind().map_or("unset", StatusLineType::as_str);
        if self.kind.set(kind).is_err() {
            return;
        }
        self.draws_a_row.store(draws_a_row(cfg), Ordering::Relaxed);
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::StatusLineConfigured {
                kind,
                // The row's question, not the section's: a rejected value in a
                // section already switched off reserves nothing.
                row_shows_a_problem: cfg.problem_to_paint().is_some(),
                items: items_label(cfg),
                custom_items: cfg.has_custom_items(),
            },
        );
    }

    pub(crate) fn record_ok(&self, duration_ms: u64) {
        self.record(&self.ok, duration_ms);
    }

    pub(crate) fn record_failed(&self, duration_ms: u64) {
        self.record(&self.failed, duration_ms);
    }

    pub(crate) fn record_timed_out(&self, duration_ms: u64) {
        self.record(&self.timed_out, duration_ms);
    }

    /// A task that never answered rather than a slow one, so there is no
    /// duration to record.
    pub(crate) fn record_abandoned(&self) {
        self.abandoned.fetch_add(1, Ordering::Relaxed);
    }

    fn record(&self, counter: &AtomicU64, duration_ms: u64) {
        counter.fetch_add(1, Ordering::Relaxed);
        self.slowest_ms.fetch_max(duration_ms, Ordering::Relaxed);
    }

    pub(crate) fn report_health(&self) {
        if let Some(event) = self.health_event() {
            echo_agent_telemetry::session_ctx::log_event(event);
        }
    }

    /// `None` when there is nothing to report: both exit paths call this and
    /// the first wins, and a session with no row would dilute the signal.
    fn health_event(&self) -> Option<echo_agent_telemetry::events::StatusLineHealth> {
        if !self.draws_a_row.load(Ordering::Relaxed) {
            return None;
        }
        // Read before the latch: a session is allowed one report, and a swap
        // above this would spend it on the event the `?` goes on to drop.
        let kind = self.kind.get()?;
        if self.reported.swap(true, Ordering::Relaxed) {
            return None;
        }
        Some(echo_agent_telemetry::events::StatusLineHealth {
            kind,
            had_content: self.had_content.load(Ordering::Relaxed),
            runs_ok: self.ok.load(Ordering::Relaxed),
            runs_failed: self.failed.load(Ordering::Relaxed),
            runs_timed_out: self.timed_out.load(Ordering::Relaxed),
            runs_abandoned: self.abandoned.load(Ordering::Relaxed),
            slowest_ms: self.slowest_ms.load(Ordering::Relaxed),
        })
    }
}

/// The segments a builtin row draws, empty for every other mode.
fn items_label(cfg: &StatusLineConfig) -> String {
    match cfg.resolve() {
        Some(ResolvedStatusLine::Builtin { items }) => items
            .iter()
            .copied()
            .map(StatusLineItem::as_str)
            .collect::<Vec<_>>()
            .join(","),
        Some(ResolvedStatusLine::Command { .. }) | None => String::new(),
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
