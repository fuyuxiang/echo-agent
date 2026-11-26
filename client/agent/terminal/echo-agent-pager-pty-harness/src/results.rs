//! Aggregated benchmark results, percentile computation, baseline compare.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::timing::FrameTiming;

/// Aggregated benchmark results for a single scenario run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResults {
    pub scenario: String,
    pub total_frames: u64,
    pub avg_fps: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub jank_count: u64,
    pub jank_rate: f64,
    pub chars_per_frame_avg: f64,
}

impl BenchResults {
    /// Compute aggregate statistics from per-frame timings.
    ///
    /// `wall_time` is the total elapsed time during which frames were collected,
    /// used to compute `avg_fps`. This should be measured by the caller.
    pub fn from_timings(scenario: &str, timings: &[FrameTiming], wall_time: Duration) -> Self {
        let total_frames = timings.len() as u64;

        if timings.is_empty() {
            return Self {
                scenario: scenario.to_owned(),
                total_frames: 0,
                avg_fps: 0.0,
                p50_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
                jank_count: 0,
                jank_rate: 0.0,
                chars_per_frame_avg: 0.0,
            };
        }

        let mut durations_ms: Vec<f64> = timings
            .iter()
            .map(|t| t.duration.as_secs_f64() * 1000.0)
            .collect();
        durations_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let wall_secs = wall_time.as_secs_f64();
        let avg_fps = if wall_secs > 0.0 {
            total_frames as f64 / wall_secs
        } else {
            0.0
        };
        let p50_ms = percentile(&durations_ms, 50.0);
        let p99_ms = percentile(&durations_ms, 99.0);
        let max_ms = durations_ms.last().copied().unwrap_or(0.0);

        // Jank threshold: frame time > 2x median (p50).
        let jank_threshold = p50_ms * 2.0;
        let jank_count = durations_ms.iter().filter(|&&d| d > jank_threshold).count() as u64;
        let jank_rate = jank_count as f64 / total_frames as f64;

        let total_chars: usize = timings.iter().map(|t| t.chars).sum();
        let chars_per_frame_avg = total_chars as f64 / total_frames as f64;

        Self {
            scenario: scenario.to_owned(),
            total_frames,
            avg_fps,
            p50_ms,
            p99_ms,
            max_ms,
            jank_count,
            jank_rate,
            chars_per_frame_avg,
        }
    }
}

// ── Baseline comparison ────────────────────────────────────────────────────

/// Regression threshold: fail if a scenario's p99 frame time grows by more
/// than this fraction (0.15 = 15%). Matches the RFC's proposal.
pub const DEFAULT_REGRESSION_THRESHOLD: f64 = 0.15;

/// On-disk baseline schema: `{ "<scenario>": BenchResults, ... }`.
pub type Baseline = HashMap<String, BenchResults>;

/// Load a baseline file from disk.
pub fn load_baseline(path: &Path) -> Result<Baseline> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read baseline file {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse baseline file {}", path.display()))
}

/// Persist a set of results as a baseline file (overwrites if present).
pub fn write_baseline(path: &Path, results: &[BenchResults]) -> Result<()> {
    let map: Baseline = results
        .iter()
        .map(|r| (r.scenario.clone(), r.clone()))
        .collect();
    let json = serde_json::to_string_pretty(&map).context("serialize baseline")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create baseline parent {}", parent.display()))?;
    }
    std::fs::write(path, json)
        .with_context(|| format!("write baseline file {}", path.display()))?;
    Ok(())
}

/// Outcome of comparing a single scenario's current run against its baseline.
#[derive(Debug, Clone)]
pub struct ScenarioRegression {
    pub scenario: String,
    pub baseline_p99_ms: f64,
    pub current_p99_ms: f64,
    pub pct_delta: f64,
}

/// Compare the given `results` against `baseline`, returning every scenario
/// whose p99 grew by more than `threshold` (as a fraction, e.g. 0.15 = 15%).
///
/// Scenarios missing from the baseline are skipped (first run of a new
/// scenario is not a regression).
pub fn compare_baseline(
    results: &[BenchResults],
    baseline: &Baseline,
    threshold: f64,
) -> Vec<ScenarioRegression> {
    let mut regressions = Vec::new();
    for r in results {
        let Some(b) = baseline.get(&r.scenario) else {
            continue;
        };
        if b.p99_ms <= 0.0 {
            continue;
        }
        let pct_delta = (r.p99_ms - b.p99_ms) / b.p99_ms;
        if pct_delta > threshold {
            regressions.push(ScenarioRegression {
                scenario: r.scenario.clone(),
                baseline_p99_ms: b.p99_ms,
                current_p99_ms: r.p99_ms,
                pct_delta,
            });
        }
    }
    regressions
}

/// Compute the `pct`-th percentile from a **sorted** slice of values.
///
/// `pct` must be in `[0.0, 100.0]`. The input slice must be sorted in
/// ascending order; this is enforced by debug assertion.
pub fn percentile(sorted: &[f64], pct: f64) -> f64 {
    debug_assert!(
        (0.0..=100.0).contains(&pct),
        "percentile must be in [0.0, 100.0], got {pct}"
    );
    debug_assert!(
        sorted.windows(2).all(|w| w[0] <= w[1]),
        "input must be sorted"
    );
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (pct / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_timings_returns_zeroed() {
        let results = BenchResults::from_timings("empty", &[], Duration::from_secs(1));
        assert_eq!(results.total_frames, 0);
        assert_eq!(results.avg_fps, 0.0);
        assert_eq!(results.p50_ms, 0.0);
        assert_eq!(results.p99_ms, 0.0);
        assert_eq!(results.max_ms, 0.0);
        assert_eq!(results.jank_count, 0);
        assert_eq!(results.chars_per_frame_avg, 0.0);
    }

    #[test]
    fn single_frame_returns_correct_values() {
        let timings = vec![FrameTiming {
            duration: Duration::from_millis(16),
            chars: 100,
        }];
        let results = BenchResults::from_timings("single", &timings, Duration::from_secs(1));
        assert_eq!(results.total_frames, 1);
        assert!((results.avg_fps - 1.0).abs() < 0.01);
        assert!((results.p50_ms - 16.0).abs() < 0.1);
        assert!((results.p99_ms - 16.0).abs() < 0.1);
        assert!((results.max_ms - 16.0).abs() < 0.1);
        assert_eq!(results.jank_count, 0);
        assert!((results.chars_per_frame_avg - 100.0).abs() < 0.01);
    }

    #[test]
    fn multiple_frames_statistics() {
        let timings: Vec<FrameTiming> = (0..100)
            .map(|i| FrameTiming {
                duration: Duration::from_millis(10 + i % 5),
                chars: 50,
            })
            .collect();
        let results = BenchResults::from_timings("multi", &timings, Duration::from_secs(2));
        assert_eq!(results.total_frames, 100);
        assert!((results.avg_fps - 50.0).abs() < 0.01);
        assert!(results.p50_ms >= 10.0 && results.p50_ms <= 14.0);
        assert!(results.p99_ms >= 10.0 && results.p99_ms <= 14.0);
        assert!((results.max_ms - 14.0).abs() < 0.1);
    }

    #[test]
    fn percentile_empty_returns_zero() {
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn percentile_single_element() {
        assert_eq!(percentile(&[42.0], 50.0), 42.0);
        assert_eq!(percentile(&[42.0], 0.0), 42.0);
        assert_eq!(percentile(&[42.0], 100.0), 42.0);
    }

    #[test]
    fn percentile_multiple_elements() {
        let sorted: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        // p50 of 1..=100 should be around 50
        let p50 = percentile(&sorted, 50.0);
        assert!((p50 - 50.0).abs() < 1.1);
        // p99 should be around 99
        let p99 = percentile(&sorted, 99.0);
        assert!((p99 - 99.0).abs() < 1.1);
    }

    #[test]
    fn jank_detection() {
        // 9 frames at 10ms, 1 frame at 50ms (>2x median = jank)
        let mut timings: Vec<FrameTiming> = (0..9)
            .map(|_| FrameTiming {
                duration: Duration::from_millis(10),
                chars: 10,
            })
            .collect();
        timings.push(FrameTiming {
            duration: Duration::from_millis(50),
            chars: 10,
        });
        let results = BenchResults::from_timings("jank", &timings, Duration::from_secs(1));
        assert_eq!(results.jank_count, 1);
        assert!((results.jank_rate - 0.1).abs() < 0.01);
    }
}
