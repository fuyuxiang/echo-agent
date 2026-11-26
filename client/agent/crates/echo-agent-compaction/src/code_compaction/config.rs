//! echo-agent-build compaction configuration.
//!
//! Holds the [`FullReplaceConfig`] tunables struct (mirroring
//! [`IntraCompactionConfig`](crate::intra_compaction::IntraCompactionConfig) /
//! [`InterCompactionConfig`](crate::inter_compaction::InterCompactionConfig),
//! which also live in their module's `config.rs`) plus the shared default
//! values. Trigger *wiring* (pre-sampling checks, preflight overflow,
//! model-switch, suppression) stays per-host.

/// Default auto-compact threshold (% of context window) when no other source
/// (env var, user config, remote per-model/global flags) sets it. Shared by
/// echo-agent-build and EchoAgent chat (~85% trigger on both sides).
pub const DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT: u8 = 85;

/// Minimum character count for a cleaned summary seed.
///
/// echo-agent-build retries when the cleaned summary is shorter than this — the
/// smallest healthy prod summary observed was ~3,242 chars; anything under
/// 500 is treated as degenerate and retried like a transient failure.
pub const MIN_SUMMARY_SEED_CHARS: usize = 500;

/// Tunables for the full-replace pass.
#[derive(Debug, Clone)]
pub struct FullReplaceConfig {
    /// Total LLM attempts (first try + retries) on transient failures.
    pub max_attempts: u32,
    /// Delay between transient retries.
    pub retry_delay_secs: u64,
    /// End-to-end timeout for each compaction LLM call.
    pub sampling_timeout_secs: u64,
}

impl Default for FullReplaceConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            retry_delay_secs: 3,
            sampling_timeout_secs: 120,
        }
    }
}
