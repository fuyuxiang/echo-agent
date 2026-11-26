//! Configuration for inter-compaction.
//!
//! This is a plain data struct — harness-specific service-config integration
//! stays in the compaction subscriber, which resolves config values and
//! constructs this struct.

use serde::{Deserialize, Serialize};

use crate::history::types::CompactionStrategy;

/// Runtime configuration for a single inter-compaction invocation.
///
/// Mirrors the fields used by the between-turn compaction service config,
/// without a harness-specific config-macro dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterCompactionConfig {
    /// The agent/scheduler name to use for the compaction model.
    ///
    /// NOTE: model routing is host policy — kept here only because
    /// service configs deserialize this struct as-is; slated to move to the
    /// per-harness policy split in a later phase.
    pub compaction_model_name: String,
    /// End-to-end timeout for the compaction sampling in seconds.
    pub sampling_timeout_secs: u64,
    /// Which compaction strategy to use.
    pub compaction_strategy: CompactionStrategy,
    /// [DivideAndConquer] Max tokens per chunk before sending to the LLM.
    /// (Basic strategy ignores this and emits a single chunk.)
    pub dnc_chunk_token_limit: u32,
    /// User messages with character count > this threshold are truncated
    /// (middle-cut) when assembling the `<echo_agent_user_queries>` preamble.
    /// Applies to both Basic and DivideAndConquer.
    pub user_message_compact_threshold: u32,
}
