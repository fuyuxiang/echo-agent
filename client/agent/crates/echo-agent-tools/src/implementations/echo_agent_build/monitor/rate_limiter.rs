use std::time::{Duration, Instant};

use super::types::{AUTO_KILL_THRESHOLD_MS, RATE_LIMIT_REFILL_MS};

/// Token bucket rate limiter.
///
/// Starts full at `capacity` tokens. Each `try_consume()` takes one token.
/// Tokens refill at 1 per `refill_interval_ms`.
pub struct TokenBucket {
    capacity: u32,
    tokens: u32,
    refill_interval: Duration,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_interval_ms: u64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_interval: Duration::from_millis(refill_interval_ms),
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns true if a token was available.
    pub fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let refills = (elapsed.as_millis() / self.refill_interval.as_millis()) as u32;
        if refills > 0 {
            self.tokens = self.tokens.saturating_add(refills).min(self.capacity);
            self.last_refill += self.refill_interval * refills;
        }
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }
}

/// Tracks rate-limit suppression state and auto-kill logic.
///
/// Used alongside `TokenBucket` to detect sustained overload and generate
/// catch-up notices when the rate subsides.
#[derive(Default)]
pub struct SuppressionTracker {
    pub suppressed_count: u64,
    pub last_suppression: Option<Instant>,
    pub suppression_start: Option<Instant>,
    pub killed: bool,
    /// Resolved model-facing name for the kill tool, used in suppression notices.
    kill_tool_name: String,
}

/// Result of processing an event through the rate limiter + suppression tracker.
pub enum RateLimitOutcome {
    /// Event is allowed through. If `catch_up_notice` is Some, a suppression
    /// notice should be sent before the event.
    Allowed { catch_up_notice: Option<String> },
    /// Event is suppressed (token bucket empty).
    Suppressed,
    /// Monitor should be auto-killed (sustained overload for 30s+).
    AutoKill { message: String },
}

impl SuppressionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kill_tool_name(mut self, name: String) -> Self {
        self.kill_tool_name = name;
        self
    }

    /// Process a rate-limit decision. Call after `TokenBucket::try_consume()`.
    pub fn process(&mut self, token_available: bool, _description: &str) -> RateLimitOutcome {
        if self.killed {
            return RateLimitOutcome::Suppressed;
        }

        if token_available {
            let catch_up = if self.suppressed_count > 0 {
                let kill_name = if self.kill_tool_name.is_empty() {
                    "kill_command_or_subagent"
                } else {
                    &self.kill_tool_name
                };
                let notice = format!(
                    "[{} events suppressed -- output rate too high. \
                     Consider using {} to restart this monitor \
                     with a more selective filter.]",
                    self.suppressed_count, kill_name
                );
                self.suppressed_count = 0;

                // Reset suppression start if the burst has subsided
                // (> 3x refill interval since last suppression).
                if let Some(last) = self.last_suppression
                    && last.elapsed() > Duration::from_millis(RATE_LIMIT_REFILL_MS * 3)
                {
                    self.suppression_start = None;
                }
                Some(notice)
            } else {
                None
            };
            RateLimitOutcome::Allowed {
                catch_up_notice: catch_up,
            }
        } else {
            self.suppressed_count += 1;
            self.last_suppression = Some(Instant::now());
            if self.suppression_start.is_none() {
                self.suppression_start = Some(Instant::now());
            }

            // Check auto-kill threshold.
            if let Some(start) = self.suppression_start {
                let elapsed = start.elapsed();
                if elapsed > Duration::from_millis(AUTO_KILL_THRESHOLD_MS) {
                    self.killed = true;
                    let secs = elapsed.as_secs();
                    return RateLimitOutcome::AutoKill {
                        message: format!(
                            "[Monitor stopped -- your script produced too much output \
                             ({} events suppressed over {secs}s). \
                             Write a new monitor command that filters more aggressively -- \
                             pipe through grep --line-buffered, awk, or a wrapper script \
                             that only emits the specific events you need.]",
                            self.suppressed_count
                        ),
                    };
                }
            }

            RateLimitOutcome::Suppressed
        }
    }
}

/// Combined rate limiter: token bucket + suppression tracker.
pub struct MonitorRateLimiter {
    pub bucket: TokenBucket,
    pub suppression: SuppressionTracker,
}

impl MonitorRateLimiter {
    pub fn new(capacity: u32, refill_interval_ms: u64) -> Self {
        Self {
            bucket: TokenBucket::new(capacity, refill_interval_ms),
            suppression: SuppressionTracker::new(),
        }
    }

    pub fn with_kill_tool_name(mut self, name: String) -> Self {
        self.suppression = self.suppression.with_kill_tool_name(name);
        self
    }

    /// Process an event. Returns the rate limit decision.
    pub fn process_event(&mut self, description: &str) -> RateLimitOutcome {
        let available = self.bucket.try_consume();
        self.suppression.process(available, description)
    }

    pub fn is_killed(&self) -> bool {
        self.suppression.killed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_starts_full() {
        let mut bucket = TokenBucket::new(10, 2000);
        for _ in 0..10 {
            assert!(bucket.try_consume());
        }
        assert!(!bucket.try_consume());
    }

    #[test]
    fn bucket_refills_after_interval() {
        let mut bucket = TokenBucket::new(10, 50); // 50ms for test speed
        for _ in 0..10 {
            bucket.try_consume();
        }
        assert!(!bucket.try_consume());
        std::thread::sleep(Duration::from_millis(60));
        assert!(bucket.try_consume()); // one token refilled
    }

    #[test]
    fn bucket_does_not_exceed_capacity() {
        let mut bucket = TokenBucket::new(3, 50);
        std::thread::sleep(Duration::from_millis(200)); // enough for many refills
        // Should be capped at 3
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        assert!(!bucket.try_consume());
    }

    #[test]
    fn suppression_tracker_counts() {
        let mut tracker = SuppressionTracker::new();
        let outcome = tracker.process(false, "test");
        assert!(matches!(outcome, RateLimitOutcome::Suppressed));
        assert_eq!(tracker.suppressed_count, 1);
    }

    #[test]
    fn catch_up_notice_on_recovery() {
        let mut tracker = SuppressionTracker::new();
        // Suppress some events
        tracker.process(false, "test");
        tracker.process(false, "test");
        tracker.process(false, "test");
        assert_eq!(tracker.suppressed_count, 3);

        // Now a token is available
        let outcome = tracker.process(true, "test");
        match outcome {
            RateLimitOutcome::Allowed { catch_up_notice } => {
                let notice = catch_up_notice.expect("should have catch-up notice");
                assert!(notice.contains("3 events suppressed"));
            }
            _ => panic!("expected Allowed with catch-up notice"),
        }
        assert_eq!(tracker.suppressed_count, 0);
    }

    #[test]
    fn no_catch_up_when_no_suppression() {
        let mut tracker = SuppressionTracker::new();
        let outcome = tracker.process(true, "test");
        match outcome {
            RateLimitOutcome::Allowed { catch_up_notice } => {
                assert!(catch_up_notice.is_none());
            }
            _ => panic!("expected Allowed without catch-up"),
        }
    }

    #[test]
    fn killed_discards_events() {
        let mut tracker = SuppressionTracker::new();
        tracker.killed = true;
        let outcome = tracker.process(true, "test");
        assert!(matches!(outcome, RateLimitOutcome::Suppressed));
    }

    #[test]
    fn combined_rate_limiter() {
        let mut rl = MonitorRateLimiter::new(3, 2000);
        // First 3 events pass
        for _ in 0..3 {
            assert!(matches!(
                rl.process_event("test"),
                RateLimitOutcome::Allowed { .. }
            ));
        }
        // 4th is suppressed
        assert!(matches!(
            rl.process_event("test"),
            RateLimitOutcome::Suppressed
        ));
    }
}
