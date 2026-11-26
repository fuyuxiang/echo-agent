//! Bounded sliding window over `(timestamp, is_failure)` samples.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Safety cap on sliding window entries to bound memory under sustained
/// high load (e.g. 10K req/s * 60s window would otherwise reach 600K
/// entries).
pub(crate) const MAX_WINDOW_ENTRIES: usize = 10_000;

pub(crate) struct SlidingWindow {
    entries: VecDeque<(Instant, bool)>,
    /// Incremental count of `is_failure = true` entries currently in
    /// `entries`. Maintained on push/pop so `error_rate()` is O(1)
    /// instead of O(n) — avoids a per-request hot-path scan under
    /// the breaker mutex.
    failures: usize,
}

impl SlidingWindow {
    pub(crate) fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            failures: 0,
        }
    }

    pub(crate) fn push(&mut self, is_failure: bool, at: Instant) {
        if self.entries.len() >= MAX_WINDOW_ENTRIES
            && let Some((_, was_failure)) = self.entries.pop_front()
            && was_failure
        {
            self.failures -= 1;
        }
        self.entries.push_back((at, is_failure));
        if is_failure {
            self.failures += 1;
        }
    }

    pub(crate) fn evict(&mut self, window: Duration, now: Instant) {
        let Some(cutoff) = now.checked_sub(window) else {
            return;
        };
        while let Some(&(ts, was_failure)) = self.entries.front() {
            if ts < cutoff {
                self.entries.pop_front();
                if was_failure {
                    self.failures -= 1;
                }
            } else {
                break;
            }
        }
    }

    pub(crate) fn error_rate(&self) -> f64 {
        if self.entries.is_empty() {
            return 0.0;
        }
        self.failures as f64 / self.entries.len() as f64
    }

    pub(crate) fn sample_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.failures = 0;
    }
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
