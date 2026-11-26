//! OS resource snapshots for soak tests, read through `echo_agent_tty_utils` so the
//! soaks and production measure the same way.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// RSS in bytes, live threads, and open files, sampled together. `None` marks a
/// metric the platform can't report.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceSnapshot {
    pub rss: Option<usize>,
    pub threads: Option<usize>,
    pub open_files: Option<usize>,
}

/// Saturating per-field growth of one [`ResourceSnapshot`] over an earlier
/// baseline. A distinct type from a snapshot so a delta can't be mistaken for
/// an absolute sample. `None` marks a field either side couldn't report.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceGrowth {
    pub rss: Option<usize>,
    pub threads: Option<usize>,
    pub open_files: Option<usize>,
}

impl ResourceSnapshot {
    pub fn capture() -> Self {
        let usage = echo_agent_tty_utils::sample_process_resources();
        let widen = |value: Option<u64>| value.map(|n| n as usize);
        Self {
            rss: widen(usage.rss_bytes),
            threads: widen(usage.threads),
            open_files: widen(usage.open_files),
        }
    }

    /// RSS from the cheap sampler, skipping the Linux descriptor scan (the
    /// thread gauge rides that sample either way and is dropped here). For
    /// sampling loops that read just `rss`.
    pub fn capture_rss() -> Option<usize> {
        echo_agent_tty_utils::sample_process_memory()
            .rss_bytes
            .map(|n| n as usize)
    }

    /// Growth of `self` (after) over `baseline` (before); see [`ResourceGrowth`].
    pub fn growth_from(&self, baseline: &ResourceSnapshot) -> ResourceGrowth {
        let delta = |after: Option<usize>, before: Option<usize>| {
            before.zip(after).map(|(b, a)| a.saturating_sub(b))
        };
        ResourceGrowth {
            rss: delta(self.rss, baseline.rss),
            threads: delta(self.threads, baseline.threads),
            open_files: delta(self.open_files, baseline.open_files),
        }
    }
}

const BYTES_PER_MB: u64 = 1024 * 1024;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(3);

/// RSS baseline plus a background polling thread for one measured section.
pub struct RssSampler {
    baseline: Option<usize>,
    stop: Arc<AtomicBool>,
    /// `Option` so `finish` can move the handle out despite `Drop`.
    handle: Option<JoinHandle<usize>>,
}

/// Stops the polling thread even when a panic skips `finish`.
impl Drop for RssSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl RssSampler {
    /// Capture the RSS baseline and start a background sampler.
    pub fn start() -> Self {
        let baseline = ResourceSnapshot::capture_rss();
        let stop = Arc::new(AtomicBool::new(false));
        let sampler_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut peak = ResourceSnapshot::capture_rss().unwrap_or(0);
            while !sampler_stop.load(Ordering::Relaxed) {
                if let Some(r) = ResourceSnapshot::capture_rss() {
                    peak = peak.max(r);
                }
                std::thread::sleep(SAMPLE_INTERVAL);
            }
            if let Some(r) = ResourceSnapshot::capture_rss() {
                peak = peak.max(r);
            }
            peak
        });
        Self {
            baseline,
            stop,
            handle: Some(handle),
        }
    }

    /// Stop sampling and return the measurement. A final synchronous read
    /// backstops a peak that rose and fell between sampler ticks.
    pub fn finish(mut self) -> RssMeasurement {
        let final_rss = ResourceSnapshot::capture_rss().unwrap_or(0);
        self.stop.store(true, Ordering::Relaxed);
        let handle = self.handle.take().expect("finish is the only taker");
        let peak_rss = handle.join().expect("sampler thread").max(final_rss);
        RssMeasurement {
            baseline: self.baseline,
            peak_rss,
        }
    }
}

/// Baseline and peak captured by one sampler run, separate from any budget so
/// one measurement can be judged against several.
pub struct RssMeasurement {
    pub baseline: Option<usize>,
    pub peak_rss: usize,
}

impl RssMeasurement {
    pub fn against_budget(self, budget_mb: u64) -> RssOutcome {
        RssOutcome {
            baseline: self.baseline,
            peak_rss: self.peak_rss,
            budget_mb,
        }
    }
}

/// Peak-RSS growth over a baseline, judged against a budget. Unmeasurable RSS
/// fails the verdict rather than passing vacuously.
pub struct RssOutcome {
    pub baseline: Option<usize>,
    pub peak_rss: usize,
    pub budget_mb: u64,
}

impl RssOutcome {
    pub fn baseline_bytes(&self) -> usize {
        self.baseline.unwrap_or(0)
    }

    pub fn baseline_mb(&self) -> f64 {
        self.baseline_bytes() as f64 / BYTES_PER_MB as f64
    }

    pub fn peak_rss_mb(&self) -> f64 {
        self.peak_rss as f64 / BYTES_PER_MB as f64
    }

    pub fn measurable(&self) -> bool {
        self.baseline.is_some() && self.peak_rss > 0
    }

    pub fn peak_growth_bytes(&self) -> usize {
        self.peak_rss.saturating_sub(self.baseline_bytes())
    }

    pub fn peak_growth_mb(&self) -> f64 {
        self.peak_growth_bytes() as f64 / BYTES_PER_MB as f64
    }

    pub fn within_budget(&self) -> bool {
        (self.peak_growth_bytes() as u64) < self.budget_mb * BYTES_PER_MB
    }

    pub fn pass(&self) -> bool {
        self.measurable() && self.within_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_outcome_fails_over_budget_and_when_unmeasurable() {
        let mb = BYTES_PER_MB as usize;

        let over = RssOutcome {
            baseline: Some(mb),
            peak_rss: mb + 3 * mb,
            budget_mb: 2,
        };
        assert_eq!(over.peak_growth_bytes(), 3 * mb);
        assert!(!over.pass());

        let unmeasurable = RssOutcome {
            baseline: None,
            peak_rss: 0,
            budget_mb: 1,
        };
        assert!(!unmeasurable.pass());
    }

    #[test]
    fn growth_from_saturates_and_propagates_none() {
        let before = ResourceSnapshot {
            rss: Some(100),
            threads: Some(5),
            open_files: None,
        };
        let after = ResourceSnapshot {
            rss: Some(30),
            threads: Some(9),
            open_files: Some(3),
        };
        let growth = after.growth_from(&before);
        assert_eq!(growth.rss, Some(0), "a shrink saturates to zero");
        assert_eq!(growth.threads, Some(4), "growth is the delta");
        assert_eq!(
            growth.open_files, None,
            "a missing baseline sample propagates None"
        );
    }
}
