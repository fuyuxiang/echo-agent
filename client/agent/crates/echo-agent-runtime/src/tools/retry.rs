//! Retry utilities — re-exported from `echo-agent-tools`.
//!
//! The canonical implementation now lives in `echo_agent_tools::retry`.
//! This module re-exports with backward-compatible aliases.

pub use echo_agent_tools::retry::BackoffConfig as RetryConfig;
pub use echo_agent_tools::retry::{BackoffConfig, execute_with_backoff};

use std::future::Future;
use std::time::Duration;

/// Backward-compatible wrapper around `execute_with_backoff` that uses
/// `anyhow::Error` as the error type (matching the old signature).
pub async fn execute_with_retry<T, E, EFut, R, RFut>(
    config: &RetryConfig,
    execute: E,
    on_retry: R,
) -> Result<T, anyhow::Error>
where
    E: FnMut() -> EFut,
    EFut: Future<Output = Result<T, anyhow::Error>>,
    R: FnMut(u32, u32, Duration) -> RFut,
    RFut: Future<Output = ()>,
{
    execute_with_backoff(config, execute, on_retry).await
}
