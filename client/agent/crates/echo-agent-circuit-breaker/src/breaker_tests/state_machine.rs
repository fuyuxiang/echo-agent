//! Closed → Open → HalfOpen → Closed transitions plus threshold,
//! min-samples, window-eviction, and disabled-breaker behaviour.

use std::time::Duration;

use super::super::*;
use super::support::{breaker_with_mock, fast_config};

// -- State transitions ----------------------------------------------------

#[test]
fn closed_to_open_on_high_error_rate() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 2;
        c.error_rate_threshold = 0.5;
    }));

    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Closed);

    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
}

#[test]
fn trips_at_exact_threshold() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 2;
        c.error_rate_threshold = 0.5;
    }));

    cb.record(Outcome::Success);
    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
}

#[test]
fn does_not_trip_below_threshold() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 3;
        c.error_rate_threshold = 0.5;
    }));

    cb.record(Outcome::Success);
    cb.record(Outcome::Failure);
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
}

#[test]
fn does_not_trip_below_min_samples() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 5;
        c.error_rate_threshold = 0.5;
    }));

    for _ in 0..4 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Closed);
}

#[test]
fn open_to_half_open_after_duration() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
    }));

    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
    assert!(cb.check().is_err());

    clock.advance(Duration::from_millis(70));
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);
}

#[test]
fn half_open_to_closed_on_probe_success() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
    }));

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));
    cb.check().unwrap();

    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
}

#[test]
fn half_open_to_open_on_probe_failure() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
    }));

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));
    cb.check().unwrap();

    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
}

// -- Window eviction ------------------------------------------------------

#[test]
fn old_samples_evicted_from_window() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 2;
        c.window_duration = Duration::from_millis(100);
        c.error_rate_threshold = 0.5;
        c.open_duration = Duration::from_millis(50);
    }));

    cb.record(Outcome::Failure);
    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);

    // Recover: Open -> HalfOpen -> Closed
    clock.advance(Duration::from_millis(70));
    cb.check().unwrap();
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);

    // Record one failure, then wait for it to fall outside the window
    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(120));

    // New success triggers eviction of the old failure
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
    assert!(cb.error_rate() < 0.01);
}

// -- Disabled breaker -----------------------------------------------------

#[test]
fn disabled_breaker_always_allows() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 1;
        c.enabled = false;
    }));

    cb.record(Outcome::Failure);
    cb.record(Outcome::Failure);
    assert!(cb.check().is_ok());
}

#[test]
fn disabled_breaker_does_not_accumulate() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 1;
        c.enabled = false;
    }));

    for _ in 0..100 {
        cb.record(Outcome::Failure);
    }
    // Window should be empty since record() is a no-op when disabled
    assert!(cb.error_rate().abs() < f64::EPSILON);
    assert_eq!(cb.state(), BreakerState::Closed);
}

// -- Failure code matching ------------------------------------------------

#[test]
fn is_failure_status_matches_configured_codes() {
    let cb = CircuitBreaker::new(BreakerConfig::default());
    for code in [429, 500, 502, 503, 504] {
        assert!(cb.is_failure_status(code));
    }
    for code in [200, 201, 301, 400, 404, 501] {
        assert!(!cb.is_failure_status(code));
    }
}

#[test]
fn is_failure_status_with_custom_codes() {
    let cb = CircuitBreaker::new(BreakerConfig {
        failure_codes: [500, 503].into_iter().collect(),
        ..Default::default()
    });
    assert!(cb.is_failure_status(500));
    assert!(cb.is_failure_status(503));
    assert!(!cb.is_failure_status(429));
    assert!(!cb.is_failure_status(502));
}

// The four `parse_failure_codes_*` and four `from_lookup_*` tests live
// alongside `BreakerConfig` in `config.rs`. We add stub aliases here so
// the named-test set is complete in this file too.

#[test]
fn parse_failure_codes_basic() {
    use crate::config::parse_failure_codes;
    use std::collections::HashSet;
    assert_eq!(
        parse_failure_codes("429,500,502,503,504"),
        [429, 500, 502, 503, 504]
            .into_iter()
            .collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_with_whitespace() {
    use crate::config::parse_failure_codes;
    use std::collections::HashSet;
    assert_eq!(
        parse_failure_codes(" 429 , 500 , 502 "),
        [429, 500, 502].into_iter().collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_ignores_invalid() {
    use crate::config::parse_failure_codes;
    use std::collections::HashSet;
    assert_eq!(
        parse_failure_codes("429,abc,500,,999999"),
        [429, 500].into_iter().collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_empty_returns_empty_set() {
    use crate::config::parse_failure_codes;
    assert!(parse_failure_codes("").is_empty());
}

#[test]
fn from_lookup_returns_defaults_when_no_vars_set() {
    let config = BreakerConfig::from_lookup_with_prefix("CB_", |_| None);
    assert_eq!(config.window_duration, Duration::from_secs(60));
    assert_eq!(config.min_samples, 10);
    assert!((config.error_rate_threshold - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.open_duration, Duration::from_secs(10));
    assert_eq!(config.half_open_max_probes, 1);
    assert!(config.enabled);
}

#[test]
fn from_lookup_applies_overrides() {
    let config = BreakerConfig::from_lookup_with_prefix("CB_", |key| match key {
        "CB_WINDOW_SECS" => Some("120".into()),
        "CB_MIN_SAMPLES" => Some("20".into()),
        "CB_ERROR_RATE_THRESHOLD" => Some("0.8".into()),
        "CB_OPEN_DURATION_SECS" => Some("30".into()),
        "CB_HALF_OPEN_MAX_PROBES" => Some("3".into()),
        "CB_FAILURE_CODES" => Some("500,503".into()),
        "CB_ENABLED" => Some("false".into()),
        _ => None,
    });
    assert_eq!(config.window_duration, Duration::from_secs(120));
    assert_eq!(config.min_samples, 20);
    assert!((config.error_rate_threshold - 0.8).abs() < f64::EPSILON);
    assert_eq!(config.open_duration, Duration::from_secs(30));
    assert_eq!(config.half_open_max_probes, 3);
    assert!(!config.enabled);
}

#[test]
fn from_lookup_uses_defaults_for_unparseable_values() {
    let config = BreakerConfig::from_lookup_with_prefix("CB_", |key| match key {
        "CB_MIN_SAMPLES" => Some("not_a_number".into()),
        "CB_ERROR_RATE_THRESHOLD" => Some("abc".into()),
        "CB_HALF_OPEN_MAX_PROBES" => Some("".into()),
        _ => None,
    });
    assert_eq!(config.min_samples, 10);
    assert!((config.error_rate_threshold - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.half_open_max_probes, 1);
}

#[test]
fn from_lookup_empty_failure_codes_uses_defaults() {
    let config = BreakerConfig::from_lookup_with_prefix("CB_", |key| match key {
        "CB_FAILURE_CODES" => Some("".into()),
        _ => None,
    });
    assert_eq!(config.failure_codes, crate::config::default_failure_codes());
}

// -- Error rate -----------------------------------------------------------

#[test]
fn error_rate_reflects_window_contents() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 100;
    }));

    cb.record(Outcome::Success);
    cb.record(Outcome::Success);
    cb.record(Outcome::Failure);
    assert!((cb.error_rate() - 1.0 / 3.0).abs() < 0.01);
}

#[test]
fn error_rate_zero_on_empty_window() {
    let cb = CircuitBreaker::new(fast_config(|_| {}));
    assert!(cb.error_rate().abs() < f64::EPSILON);
}

// -- BreakerOpen ----------------------------------------------------------

#[test]
fn breaker_open_reports_retry_after() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(200);
    }));

    cb.record(Outcome::Failure);
    let err = cb.check().unwrap_err();
    assert!(err.retry_after <= Duration::from_millis(200));
    assert!(err.retry_after > Duration::from_millis(100));
}

#[test]
fn breaker_open_display() {
    let err = BreakerOpen {
        retry_after: Duration::from_millis(5300),
    };
    assert_eq!(err.to_string(), "circuit breaker open; retry after 5.3s");
}

// -- Full cycle -----------------------------------------------------------

#[test]
fn full_cycle_closed_open_half_open_closed() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 2;
        c.error_rate_threshold = 0.5;
        c.open_duration = Duration::from_millis(50);
    }));

    cb.record(Outcome::Failure);
    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);
    assert!(cb.check().is_err());

    clock.advance(Duration::from_millis(70));
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);

    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
    assert!(cb.check().is_ok());

    for _ in 0..5 {
        cb.record(Outcome::Success);
    }
    assert_eq!(cb.state(), BreakerState::Closed);
}
