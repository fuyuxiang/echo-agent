//! Generic retry utilities with exponential backoff.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::sleep;

/// Configuration for retry behavior with exponential backoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackoffConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        }
    }
}

impl BackoffConfig {
    pub fn new(max_retries: u32, base_delay_ms: u64, max_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
            max_delay_ms,
        }
    }

    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        let delay_ms = std::cmp::min(
            self.base_delay_ms * 2u64.pow(attempt.saturating_sub(1)),
            self.max_delay_ms,
        );
        Duration::from_millis(delay_ms)
    }
}

/// Execute with retry logic and exponential backoff.
/// Calls `on_retry(attempt, max_retries, delay)` before each retry.
pub async fn execute_with_backoff<T, E, EFut, R, RFut>(
    config: &BackoffConfig,
    mut execute: E,
    mut on_retry: R,
) -> Result<T, E::Error>
where
    E: ExecuteFn<T, Fut = EFut>,
    EFut: Future<Output = Result<T, E::Error>>,
    E::Error: std::fmt::Display,
    R: FnMut(u32, u32, Duration) -> RFut,
    RFut: Future<Output = ()>,
{
    let mut attempt = 0u32;
    #[allow(unused_assignments)]
    let mut last_error: Option<E::Error> = None;

    loop {
        attempt += 1;

        match execute.call().await {
            Ok(output) => {
                if attempt > 1 {
                    tracing::info!("Execution succeeded after {} attempts", attempt);
                }
                return Ok(output);
            }
            Err(err) => {
                let delay = config.calculate_delay(attempt);
                tracing::info!(
                    "Attempt {} failed ({}), retrying in {}ms",
                    attempt,
                    err,
                    delay.as_millis()
                );
                last_error = Some(err);

                if attempt >= config.max_retries {
                    tracing::warn!("Execution failed after {} attempts", attempt);
                    break;
                }

                on_retry(attempt, config.max_retries, delay).await;
                sleep(delay).await;
            }
        }
    }

    Err(last_error.unwrap())
}

/// Helper trait to work around FnMut closure limitations with async.
pub trait ExecuteFn<T> {
    type Error;
    type Fut: Future<Output = Result<T, Self::Error>>;
    fn call(&mut self) -> Self::Fut;
}

impl<T, E, Fut, F> ExecuteFn<T> for F
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    type Error = E;
    type Fut = Fut;
    fn call(&mut self) -> Self::Fut {
        (self)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_config_default() {
        let config = BackoffConfig::default();
        assert_eq!(config.max_retries, 10);
        assert_eq!(config.base_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 30_000);
    }

    #[test]
    fn test_calculate_delay() {
        let config = BackoffConfig::default();
        assert_eq!(config.calculate_delay(1), Duration::from_millis(1000));
        assert_eq!(config.calculate_delay(2), Duration::from_millis(2000));
        assert_eq!(config.calculate_delay(3), Duration::from_millis(4000));
        assert_eq!(config.calculate_delay(6), Duration::from_millis(30000)); // capped
    }

    #[test]
    fn test_calculate_delay_custom_config() {
        let config = BackoffConfig::new(5, 500, 5000);
        assert_eq!(config.calculate_delay(1), Duration::from_millis(500));
        assert_eq!(config.calculate_delay(4), Duration::from_millis(4000));
        assert_eq!(config.calculate_delay(5), Duration::from_millis(5000)); // capped
    }

    #[test]
    fn test_backoff_config_serde_roundtrip() {
        let config = BackoffConfig::new(3, 500, 5000);
        let json = serde_json::to_string(&config).unwrap();
        let restored: BackoffConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.max_retries, 3);
        assert_eq!(restored.base_delay_ms, 500);
        assert_eq!(restored.max_delay_ms, 5000);
    }
}
