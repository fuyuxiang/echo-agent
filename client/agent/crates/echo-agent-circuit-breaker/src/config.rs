//! [`BreakerConfig`] — tuning knobs for [`crate::CircuitBreaker`].
//!
//! Two named presets:
//! - [`BreakerConfig::server`] — defaults suited to a shared server-side
//!   breaker (stricter trip threshold, short cool-down).
//! - [`BreakerConfig::client`] — defaults suited to client-side breakers
//!   keyed per endpoint or tenant (fewer samples, longer cool-down).
//!
//! [`BreakerConfig::from_env`] reads `CB_*` env vars.

use std::collections::HashSet;
use std::time::Duration;

const DEFAULT_FAILURE_CODES: &[u16] = &[429, 500, 502, 503, 504];

#[derive(Debug, Clone)]
pub struct BreakerConfig {
    pub window_duration: Duration,
    pub min_samples: usize,
    pub error_rate_threshold: f64,
    pub open_duration: Duration,
    pub half_open_max_probes: usize,
    pub failure_codes: HashSet<u16>,
    pub enabled: bool,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self::server()
    }
}

impl BreakerConfig {
    /// Server preset (`min_samples=10`, `error_rate=0.5`, 60s window,
    /// 10s open duration, failure codes `[429,500,502,503,504]`).
    pub fn server() -> Self {
        Self {
            window_duration: Duration::from_secs(60),
            min_samples: 10,
            error_rate_threshold: 0.5,
            open_duration: Duration::from_secs(10),
            half_open_max_probes: 1,
            failure_codes: default_failure_codes(),
            enabled: true,
        }
    }

    /// Client preset (`min_samples=5`, `error_rate=0.5`, 60s window,
    /// 60s open duration, failure codes `[401]`).
    pub fn client() -> Self {
        Self {
            window_duration: Duration::from_secs(60),
            min_samples: 5,
            error_rate_threshold: 0.5,
            open_duration: Duration::from_secs(60),
            half_open_max_probes: 1,
            failure_codes: [401].into_iter().collect(),
            enabled: true,
        }
    }

    /// Load knobs from `CB_*` environment variables.
    pub fn from_env() -> Self {
        Self::from_env_with_prefix("CB_")
    }

    /// Load knobs from `<prefix>...` environment variables.
    pub fn from_env_with_prefix(prefix: &str) -> Self {
        Self::from_lookup_with_prefix(prefix, |key| std::env::var(key).ok())
    }

    pub(crate) fn from_lookup_with_prefix(
        prefix: &str,
        get: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let key = |k: &str| format!("{prefix}{k}");
        let failure_codes = match get(&key("FAILURE_CODES")) {
            Some(raw) => {
                let codes = parse_failure_codes(&raw);
                if codes.is_empty() {
                    log::warn!(
                        "{}FAILURE_CODES={raw:?} produced no valid codes, using defaults",
                        prefix
                    );
                    default_failure_codes()
                } else {
                    codes
                }
            }
            None => default_failure_codes(),
        };

        Self {
            window_duration: Duration::from_secs(lookup_or(&get, &key("WINDOW_SECS"), 60)),
            min_samples: lookup_or(&get, &key("MIN_SAMPLES"), 10),
            error_rate_threshold: lookup_or(&get, &key("ERROR_RATE_THRESHOLD"), 0.5),
            open_duration: Duration::from_secs(lookup_or(&get, &key("OPEN_DURATION_SECS"), 10)),
            half_open_max_probes: lookup_or(&get, &key("HALF_OPEN_MAX_PROBES"), 1usize).max(1),
            failure_codes,
            enabled: lookup_or(&get, &key("ENABLED"), true),
        }
    }

    /// `true` if `status` is in the configured failure code set.
    pub fn is_failure_status(&self, status: u16) -> bool {
        self.failure_codes.contains(&status)
    }
}

fn lookup_or<T: std::str::FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
) -> T {
    match get(key) {
        Some(v) => match v.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                log::warn!("env {key}={v:?} failed to parse, using default");
                default
            }
        },
        None => default,
    }
}

/// Default set of HTTP failure codes (`429`, `500`, `502`, `503`, `504`).
pub fn default_failure_codes() -> HashSet<u16> {
    DEFAULT_FAILURE_CODES.iter().copied().collect()
}

/// Parse a comma-separated list of status codes; invalid entries are
/// silently dropped.
pub fn parse_failure_codes(s: &str) -> HashSet<u16> {
    s.split(',').filter_map(|c| c.trim().parse().ok()).collect()
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
