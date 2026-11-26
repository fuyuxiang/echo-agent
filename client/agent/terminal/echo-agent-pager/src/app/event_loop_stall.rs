use std::time::{Duration, Instant};

use echo_agent_telemetry::events::EventLoopStall;

pub(crate) const STALL_REPORT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StallActivity {
    pub(crate) compaction_active: bool,
    pub(crate) subagents_active: u32,
    pub(crate) mcp_servers_connected: u32,
}

impl StallActivity {
    pub(crate) fn read() -> Self {
        use echo_agent_telemetry::activity;
        Self {
            compaction_active: activity::COMPACTIONS_ACTIVE.get() > 0,
            subagents_active: activity::SUBAGENTS_ACTIVE.get(),
            mcp_servers_connected: activity::MCP_SERVERS_CONNECTED.get(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StallWindow {
    pub(crate) max_stall_ms: u64,
    pub(crate) window_ms: u64,
    pub(crate) events_handled: u32,
    pub(crate) activity: StallActivity,
}

pub(super) fn event_loop_stall_event(window: StallWindow) -> EventLoopStall {
    EventLoopStall {
        max_stall_ms: window.max_stall_ms,
        window_ms: window.window_ms,
        events_handled: window.events_handled,
        stall_compaction_active: window.activity.compaction_active,
        stall_subagents_active: window.activity.subagents_active,
        stall_mcp_servers_connected: window.activity.mcp_servers_connected,
    }
}

pub(crate) fn input_wait(
    arrived_at: Instant,
    handled_at: Instant,
    loop_entry: Instant,
) -> Duration {
    handled_at.saturating_duration_since(arrived_at.max(loop_entry))
}

/// Rolls per-event input stalls up into one [`StallWindow`] per reporting
/// window, keeping only the worst stall (and the activity snapshot captured at
/// that worst moment) plus the count of events handled.
///
/// The window opens lazily on the first `observe`, so an idle loop opens no
/// window and arms no flush wakeup. Flushing preserves the boundary invariants:
/// the loop observes first and only then flushes ([`Self::take_if_elapsed`]),
/// so a boundary stall is folded into the elapsed window rather than starting a
/// new one — the window is never split and the elapsed flush never starves.
pub(crate) struct StallRollup {
    window: Duration,
    // None until the first observe, so an idle loop opens no window and arms no
    // flush wakeup.
    window_started_at: Option<Instant>,
    max_stall: Duration,
    max_stall_activity: StallActivity,
    count: u32,
}

impl StallRollup {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            window_started_at: None,
            max_stall: Duration::ZERO,
            max_stall_activity: StallActivity::default(),
            count: 0,
        }
    }

    pub(crate) fn observe(
        &mut self,
        stall: Duration,
        activity: StallActivity,
        events_handled: u32,
        now: Instant,
    ) -> Option<StallWindow> {
        let started = *self.window_started_at.get_or_insert(now);
        if stall > self.max_stall {
            self.max_stall = stall;
            self.max_stall_activity = activity;
        }
        self.count = self.count.saturating_add(events_handled);
        if now.saturating_duration_since(started) >= self.window {
            self.flush(now)
        } else {
            None
        }
    }

    pub(crate) fn take_if_elapsed(&mut self, now: Instant) -> Option<StallWindow> {
        let started = self.window_started_at?;
        if now.saturating_duration_since(started) >= self.window {
            self.flush(now)
        } else {
            None
        }
    }

    pub(crate) fn take(&mut self) -> Option<StallWindow> {
        self.flush(Instant::now())
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.window_started_at.map(|started| started + self.window)
    }

    fn flush(&mut self, now: Instant) -> Option<StallWindow> {
        let started = self.window_started_at.take()?;
        let window = StallWindow {
            max_stall_ms: duration_ms(self.max_stall),
            window_ms: duration_ms(now.saturating_duration_since(started)),
            events_handled: self.count,
            activity: self.max_stall_activity,
        };
        self.max_stall = Duration::ZERO;
        self.max_stall_activity = StallActivity::default();
        self.count = 0;
        Some(window)
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "event_loop_stall_tests.rs"]
mod tests;
