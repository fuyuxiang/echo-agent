//! Breaker state types: state enum, outcome enum, and the `BreakerOpen`
//! error returned by [`crate::CircuitBreaker::check`] when the breaker is
//! refusing traffic.

use std::time::Duration;

/// Tri-state circuit-breaker status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BreakerState {
    Closed = 0,
    Open = 1,
    HalfOpen = 2,
}

impl BreakerState {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Closed,
            1 => Self::Open,
            2 => Self::HalfOpen,
            invalid => {
                debug_assert!(false, "invalid BreakerState: {invalid}");
                Self::Closed
            }
        }
    }
}

/// Outcome of a wire request fed back to the breaker via
/// [`crate::CircuitBreaker::record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

/// Returned by [`crate::CircuitBreaker::check`] when the breaker is open
/// or has already exhausted its half-open probe slots.
#[derive(Debug)]
pub struct BreakerOpen {
    pub retry_after: Duration,
}

impl std::fmt::Display for BreakerOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "circuit breaker open; retry after {:.1}s",
            self.retry_after.as_secs_f64()
        )
    }
}

impl std::error::Error for BreakerOpen {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_open_display() {
        let err = BreakerOpen {
            retry_after: Duration::from_millis(5300),
        };
        assert_eq!(err.to_string(), "circuit breaker open; retry after 5.3s");
    }
}
