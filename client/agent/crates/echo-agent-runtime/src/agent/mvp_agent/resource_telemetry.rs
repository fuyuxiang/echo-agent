//! What the process is holding, at session close and on a bounded cadence.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use echo_agent_telemetry::events::ResourceReportTrigger;

use super::*;

/// Floor between growth-driven reports, so a fast climb costs a bounded number
/// of events.
const MIN_REPORT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A quiet process still reports this often, so a flat line is evidence rather
/// than absence.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Fraction of the last reading that counts as movement worth an event.
const MATERIAL_CHANGE_DIVISOR: u64 = 10;

/// Keeps the periodic reports rare while a process is behaving, and frequent
/// while it is not.
#[derive(Default)]
struct ReportCadence {
    last: Option<Report>,
}

// The gauges describe the process, so the cadence that rate-limits them belongs
// to the process, not to any one agent. `//` comments: `///` above
// `thread_local!` trips `clippy::unused_doc_comments`.
thread_local! {
    static CADENCE: RefCell<ReportCadence> = RefCell::new(ReportCadence::default());
}

struct Report {
    at: Instant,
    rss_bytes: Option<u64>,
}

impl ReportCadence {
    fn is_due(&self, now: Instant, rss_bytes: Option<u64>) -> bool {
        let Some(last) = &self.last else {
            return true;
        };
        let elapsed = now.saturating_duration_since(last.at);
        if elapsed >= HEARTBEAT_INTERVAL {
            return true;
        }
        if elapsed < MIN_REPORT_INTERVAL {
            return false;
        }
        match (rss_bytes, last.rss_bytes) {
            (Some(current), Some(previous)) => {
                current.abs_diff(previous) >= previous / MATERIAL_CHANGE_DIVISOR
            }
            _ => false,
        }
    }

    fn record(&mut self, at: Instant, rss_bytes: Option<u64>) {
        self.last = Some(Report { at, rss_bytes });
    }
}

impl MvpAgent {
    /// Sampled after removal, so a leak reads as a rising tail across releases.
    pub(super) fn log_resource_usage(&self, trigger: ResourceReportTrigger) {
        let usage = echo_agent_tty_utils::sample_process_resources();
        CADENCE.with_borrow_mut(|cadence| cadence.record(Instant::now(), usage.rss_bytes));
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::ProcessResourceUsage {
                trigger,
                rss_bytes: usage.rss_bytes,
                peak_rss_bytes: usage.peak_rss_bytes,
                footprint_bytes: usage.footprint_bytes,
                allocated_bytes: crate::heap_profile::stats().map(|stats| stats.allocated),
                threads: usage.threads,
                open_files: usage.open_files,
                resident_sessions: self.session_registry.resident_count(),
                session_threads: self.session_registry.counts().session_threads,
            },
        );
    }

    /// Called from the heap monitor's poll loop, which runs whether or not
    /// profiling is on. A session that never closes reports through this.
    ///
    /// Every tick pays one memory read, which now carries the thread count
    /// (free from the same `/proc/self/status` read on Linux, one
    /// `proc_pidinfo` on macOS); only a tick that reports pays for the
    /// descriptor scan.
    pub(super) fn report_resource_usage_if_due(&self) {
        let memory = echo_agent_tty_utils::sample_process_memory();
        let due = CADENCE.with_borrow(|cadence| cadence.is_due(Instant::now(), memory.rss_bytes));
        if due {
            self.log_resource_usage(ResourceReportTrigger::Periodic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn after(rss_bytes: u64) -> (Instant, ReportCadence) {
        let base = Instant::now();
        let mut cadence = ReportCadence::default();
        cadence.record(base, Some(rss_bytes));
        (base, cadence)
    }

    #[test]
    fn the_first_report_is_always_due() {
        assert!(ReportCadence::default().is_due(Instant::now(), Some(100)));
    }

    #[test]
    fn a_quiet_process_still_reports_hourly() {
        let (base, cadence) = after(1_000);
        assert!(cadence.is_due(at(base, 60 * 60), Some(1_000)));
    }

    #[test]
    fn movement_reports_once_past_the_floor() {
        let (base, cadence) = after(1_000);

        assert!(
            !cadence.is_due(at(base, 60), Some(2_000)),
            "the floor bounds what a fast climb can cost"
        );
        assert!(cadence.is_due(at(base, 6 * 60), Some(1_200)));
        assert!(
            !cadence.is_due(at(base, 6 * 60), Some(1_050)),
            "under a tenth is noise"
        );
        assert!(
            cadence.is_due(at(base, 6 * 60), Some(800)),
            "a purge is as worth reporting as a leak"
        );
    }

    #[test]
    fn an_unreadable_gauge_falls_back_to_the_heartbeat() {
        let base = Instant::now();
        let mut cadence = ReportCadence::default();
        cadence.record(base, None);

        assert!(!cadence.is_due(at(base, 30 * 60), None));
        assert!(cadence.is_due(at(base, 60 * 60), None));
    }
}
