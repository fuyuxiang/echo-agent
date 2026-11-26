//! Parity tests for the `server` and `client` presets, including a
//! sustained high-401-rate failure pattern.

use std::time::Duration;

use super::super::*;
use super::support::breaker_with_mock;

#[test]
fn client_preset_trips_on_5x_401s() {
    // With the sliding-window algorithm, 5 × 401 against `client()`
    // gives sample_count=5 >= min_samples=5 and rate=1.0 >= 0.5.
    let cb = CircuitBreaker::new(BreakerConfig::client());
    for _ in 0..4 {
        cb.record(Outcome::Failure);
        assert_eq!(cb.state(), BreakerState::Closed);
    }
    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
    assert!(cb.is_open());
}

#[test]
fn breaker_half_open_after_cool_down_success() {
    let (cb, clock) = breaker_with_mock(BreakerConfig::client());
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    clock.advance(Duration::from_secs(61));
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);

    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
    assert!(!cb.is_open());
}

#[test]
fn breaker_half_open_after_cool_down_failure_reopens() {
    let (cb, clock) = breaker_with_mock(BreakerConfig::client());
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    clock.advance(Duration::from_secs(61));
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);

    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
    assert!(cb.is_open());
}

#[test]
fn parity_server_trips_on_sustained_500s() {
    let cb = CircuitBreaker::new(BreakerConfig::server());
    for _ in 0..10 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);
}

#[test]
fn parity_server_does_not_trip_below_min_samples() {
    let cb = CircuitBreaker::new(BreakerConfig::server());
    for _ in 0..9 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Closed);
}

#[test]
fn parity_server_does_not_trip_below_threshold() {
    let cb = CircuitBreaker::new(BreakerConfig::server());
    // 5 failures and 6 successes interleaved (lead with the
    // successes so the partial rate never crosses 0.5 once
    // min_samples is reached): SSSSSS FFFFF → 11 samples,
    // rate = 5/11 ≈ 0.4545.
    for _ in 0..6 {
        cb.record(Outcome::Success);
    }
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Closed);
    assert!(cb.error_rate() < 0.5);
}

#[test]
fn parity_server_half_open_probe_then_close() {
    let (cb, clock) = breaker_with_mock(BreakerConfig::server());
    for _ in 0..10 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    clock.advance(BreakerConfig::server().open_duration + Duration::from_millis(1));
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);

    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
}

#[test]
fn parity_client_trips_on_fresh_session_5x_401() {
    let cb = CircuitBreaker::new(BreakerConfig::client());
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);
    assert!((cb.error_rate() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn client_preset_trips_on_interleaved_success_pattern() {
    // [401×4, 200, 401×5] = 10 samples, 9 failures, rate = 0.9.
    let cb = CircuitBreaker::new(BreakerConfig::client());
    for _ in 0..4 {
        cb.record(Outcome::Failure);
    }
    cb.record(Outcome::Success);
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);
    assert!((cb.error_rate() - 0.9).abs() < 1e-9);
}
