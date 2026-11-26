//! One instant captured on two clocks, so elapsed time stays honest across
//! a system suspend without trusting the wall clock alone.

use std::time::{Duration, Instant, SystemTime};

/// `Instant` is monotonic but, on macOS (`mach_absolute_time`) and Linux
/// (`CLOCK_MONOTONIC`), *pauses while the machine is asleep*, so it alone
/// under-reports any span containing a suspend. `SystemTime` keeps advancing
/// through sleep but jumps with NTP steps and manual changes. Capturing both
/// lets a caller bound elapsed *awake* time (mono), elapsed *real* time
/// (wall), and their difference — which grows by exactly the suspended time.
#[derive(Clone, Copy)]
pub(crate) struct DualClock {
    /// Monotonic; pauses during sleep. Bounds elapsed *awake* time.
    pub(crate) mono: Instant,
    /// Wall clock; advances through sleep. Bounds elapsed *real* time.
    pub(crate) wall: SystemTime,
}

impl DualClock {
    pub(crate) fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// Elapsed on each clock as `(monotonic, wall)`. Wall elapsed clamps to
    /// zero if the clock ran backwards (NTP step) so a backward jump can
    /// never fabricate a suspend or inflate a duration.
    pub(crate) fn elapsed_between(&self, now: DualClock) -> (Duration, Duration) {
        (
            now.mono.saturating_duration_since(self.mono),
            now.wall.duration_since(self.wall).unwrap_or(Duration::ZERO),
        )
    }

    /// [`Self::elapsed_between`] against the live clocks.
    pub(crate) fn elapsed(&self) -> (Duration, Duration) {
        self.elapsed_between(Self::now())
    }

    /// `(awake, total, suspended)` durations since this instant.
    pub(crate) fn elapsed_split(&self) -> (Duration, Duration, Duration) {
        let (awake, total) = self.elapsed();
        (awake, total, total.saturating_sub(awake))
    }
}

#[cfg(test)]
#[path = "dual_clock_tests.rs"]
mod tests;
