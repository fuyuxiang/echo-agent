//! Time source abstraction used by [`crate::CircuitBreaker`].
//!
//! Production uses [`SystemClock`]. Tests construct a [`MockClock`]
//! (gated on `cfg(test)` and the `test-hooks` feature) to drive
//! open-duration windows deterministically.

#[cfg(any(test, feature = "test-hooks"))]
use std::sync::Mutex;
#[cfg(any(test, feature = "test-hooks"))]
use std::time::Duration;
use std::time::Instant;

/// Monotonic time source.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

/// `Instant::now()`-backed clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Controllable clock: starts at construction time and only advances
/// via [`Self::advance`].
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug)]
pub struct MockClock {
    now: Mutex<Instant>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl MockClock {
    pub fn new() -> Self {
        Self {
            now: Mutex::new(Instant::now()),
        }
    }

    /// Advance the mock clock by `d`. Panics on overflow.
    pub fn advance(&self, d: Duration) {
        let mut g = self.now.lock().unwrap_or_else(|e| e.into_inner());
        *g = g.checked_add(d).expect("MockClock overflow");
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Default for MockClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Clock for MockClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }
}
