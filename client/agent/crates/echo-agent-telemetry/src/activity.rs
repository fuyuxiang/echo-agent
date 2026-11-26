//! Process-wide activity counters attached to every analytics event.

use std::sync::atomic::{AtomicU32, Ordering};

pub struct ActivityGauge(AtomicU32);

impl ActivityGauge {
    const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn dec(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    pub fn enter(&'static self) -> ActivityGaugeGuard {
        self.inc();
        ActivityGaugeGuard { gauge: self }
    }
}

#[must_use]
pub struct ActivityGaugeGuard {
    gauge: &'static ActivityGauge,
}

impl Drop for ActivityGaugeGuard {
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

pub static SUBAGENTS_ACTIVE: ActivityGauge = ActivityGauge::new();
pub static COMPACTIONS_ACTIVE: ActivityGauge = ActivityGauge::new();
pub static MCP_SERVERS_CONNECTED: ActivityGauge = ActivityGauge::new();
pub static TURNS_ACTIVE: ActivityGauge = ActivityGauge::new();
pub static WORKFLOW_RUNS_ACTIVE: ActivityGauge = ActivityGauge::new();
pub static SESSIONS_ACTIVE: ActivityGauge = ActivityGauge::new();

/// Every gauge in one read; the serde field names are the wire keys.
/// Every boundary event enters its gauge before it logs, so its own stamp
/// is self-inclusive.
#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct ActivitySnapshot {
    pub(crate) sessions_active: u32,
    pub(crate) subagents_active: u32,
    pub(crate) compaction_active: bool,
    pub(crate) mcp_servers_connected: u32,
    pub(crate) turns_active: u32,
    pub(crate) workflow_runs_active: u32,
}

impl ActivitySnapshot {
    pub(crate) fn read() -> Self {
        Self {
            sessions_active: SESSIONS_ACTIVE.get(),
            subagents_active: SUBAGENTS_ACTIVE.get(),
            compaction_active: COMPACTIONS_ACTIVE.get() > 0,
            mcp_servers_connected: MCP_SERVERS_CONNECTED.get(),
            turns_active: TURNS_ACTIVE.get(),
            workflow_runs_active: WORKFLOW_RUNS_ACTIVE.get(),
        }
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
