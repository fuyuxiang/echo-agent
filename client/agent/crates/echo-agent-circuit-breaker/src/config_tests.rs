//! Tests for [`crate::config`].

use super::*;
use std::collections::HashSet;

fn from_lookup(get: impl Fn(&str) -> Option<String>) -> BreakerConfig {
    BreakerConfig::from_lookup_with_prefix("CB_", get)
}

// -- Failure code matching ------------------------------------------------

#[test]
fn is_failure_status_matches_configured_codes() {
    let config = BreakerConfig::default();

    for code in [429, 500, 502, 503, 504] {
        assert!(
            config.is_failure_status(code),
            "expected {code} to be failure"
        );
    }
    for code in [200, 201, 301, 400, 404, 501] {
        assert!(
            !config.is_failure_status(code),
            "expected {code} to NOT be failure"
        );
    }
}

#[test]
fn is_failure_status_with_custom_codes() {
    let config = BreakerConfig {
        failure_codes: [500, 503].into_iter().collect(),
        ..Default::default()
    };
    assert!(config.is_failure_status(500));
    assert!(config.is_failure_status(503));
    assert!(!config.is_failure_status(429));
    assert!(!config.is_failure_status(502));
}

// -- parse_failure_codes --------------------------------------------------

#[test]
fn parse_failure_codes_basic() {
    assert_eq!(
        parse_failure_codes("429,500,502,503,504"),
        [429, 500, 502, 503, 504]
            .into_iter()
            .collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_with_whitespace() {
    assert_eq!(
        parse_failure_codes(" 429 , 500 , 502 "),
        [429, 500, 502].into_iter().collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_ignores_invalid() {
    assert_eq!(
        parse_failure_codes("429,abc,500,,999999"),
        [429, 500].into_iter().collect::<HashSet<_>>()
    );
}

#[test]
fn parse_failure_codes_empty_returns_empty_set() {
    assert!(parse_failure_codes("").is_empty());
}

// -- from_lookup ----------------------------------------------------------

#[test]
fn from_lookup_returns_defaults_when_no_vars_set() {
    let config = from_lookup(|_| None);
    assert_eq!(config.window_duration, Duration::from_secs(60));
    assert_eq!(config.min_samples, 10);
    assert!((config.error_rate_threshold - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.open_duration, Duration::from_secs(10));
    assert_eq!(config.half_open_max_probes, 1);
    assert_eq!(config.failure_codes, default_failure_codes());
    assert!(config.enabled);
}

#[test]
fn from_lookup_applies_overrides() {
    let config = from_lookup(|key| match key {
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
    assert_eq!(
        config.failure_codes,
        [500, 503].into_iter().collect::<HashSet<_>>()
    );
    assert!(!config.enabled);
}

#[test]
fn from_lookup_uses_defaults_for_unparseable_values() {
    let config = from_lookup(|key| match key {
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
    let config = from_lookup(|key| match key {
        "CB_FAILURE_CODES" => Some("".into()),
        _ => None,
    });
    assert_eq!(config.failure_codes, default_failure_codes());
}
