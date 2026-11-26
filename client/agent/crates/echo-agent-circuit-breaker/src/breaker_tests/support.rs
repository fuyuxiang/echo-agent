//! Shared test helpers for the `breaker_tests` sub-modules.
//!
//! Items are `pub(super)` so sibling test modules
//! (`breaker_tests::state_machine`, `breaker_tests::half_open`, …) can
//! reach them. The `#[cfg(test)] #[path = "breaker_tests/support.rs"]
//! mod support;` declaration in `breaker.rs` makes `super` resolve to
//! the parent `breaker` module — sibling sub-modules then import via
//! `use super::support::*`. A future flatten-the-`#[path]` refactor
//! would silently break those imports, hence this note.

use std::sync::Arc;
use std::time::Duration;

use super::super::CircuitBreaker;
use crate::clock::MockClock;
use crate::config::BreakerConfig;

pub(super) fn fast_config(f: impl FnOnce(&mut BreakerConfig)) -> BreakerConfig {
    let mut c = BreakerConfig {
        min_samples: 2,
        open_duration: Duration::from_millis(50),
        window_duration: Duration::from_millis(200),
        ..Default::default()
    };
    f(&mut c);
    c
}

pub(super) fn breaker_with_mock(config: BreakerConfig) -> (CircuitBreaker, Arc<MockClock>) {
    let clock = Arc::new(MockClock::new());
    let cb = CircuitBreaker::with_clock(config, clock.clone());
    (cb, clock)
}
