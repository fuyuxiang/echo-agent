//! The `CompactionSampler` seam — the LLM call that produces summaries —
//! plus its output and error types (shared failure classification).

use std::time::Duration;

use async_trait::async_trait;

use crate::prompt::CompactionPrompt;

// ---------------------------------------------------------------------------
// Sampler output + error types
// ---------------------------------------------------------------------------

/// Raw text captured from a compaction LLM call, split by channel.
///
/// Used by both intra- and inter-compaction. Intra-compaction uses only
/// `.response`; inter-compaction also persists `.thinking` for audit/debug.
#[derive(Debug, Default, Clone)]
pub struct LlmCompactionOutput {
    /// Text from the response channel — the actual compaction summary.
    pub response: String,
    /// Text from the thinking channel — the model's chain-of-thought reasoning.
    /// Stored for audit/debug only; never fed back into a conversation.
    pub thinking: String,
}

/// Error types for compaction sampling, allowing callers to distinguish
/// deterministic failures (never retry) from transient ones.
///
/// Harnesses should prefer the structured variants ([`Self::Build`],
/// [`Self::Start`], [`Self::EmptyResponse`]) so the shared retry policy can
/// classify without string matching. [`Self::Other`] remains for samplers
/// that only surface an opaque error; the orchestrator falls back to
/// matching the literal messages produced by the EchoAgent chat sampler —
/// keep those literals in sync (the `compaction_sample_error_to_intra*`
/// tests guard the mapping).
#[derive(Debug)]
pub enum CompactionSampleError {
    /// The sampler hit its end-to-end timeout. Transient.
    Timeout {
        timeout_secs: u64,
        collected_bytes: usize,
    },
    /// Sampler construction failed (bad config, unknown model). Deterministic.
    Build(String),
    /// The sampling call could not be started.
    ///
    /// Classification is asymmetric for pre-migration parity: the *inter*
    /// retry policy ([`Self::is_deterministic`]) treats it as deterministic
    /// (no retry), while the *intra* orchestrator maps it to
    /// `IntraCompactionError::SamplerStart` which its retry loop treats as
    /// transient.
    Start(String),
    /// The model produced no response-channel content. Transient.
    EmptyResponse,
    /// Anything else — classified by string matching for backward
    /// compatibility with samplers that pre-date the structured variants.
    Other(anyhow::Error),
}

impl std::fmt::Display for CompactionSampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout {
                timeout_secs,
                collected_bytes,
            } => write!(
                f,
                "Compaction sampling timed out after {}s (collected {} bytes so far)",
                timeout_secs, collected_bytes
            ),
            Self::Build(msg) => write!(f, "Compaction sampler build failed: {}", msg),
            Self::Start(msg) => write!(f, "Compaction sampler start failed: {}", msg),
            // Keep the "no response channel content" literal — the intra
            // orchestrator's `Other(_)` fallback string-matches it.
            Self::EmptyResponse => {
                write!(f, "Compaction sampler returned no response channel content")
            }
            Self::Other(e) => write!(f, "{}", e),
        }
    }
}

impl From<anyhow::Error> for CompactionSampleError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl CompactionSampleError {
    /// Whether this error is deterministic — retrying with the same input
    /// will produce the same failure.
    pub fn is_deterministic(&self) -> bool {
        match self {
            Self::Timeout { .. } | Self::EmptyResponse => false,
            Self::Build(_) | Self::Start(_) => true,
            Self::Other(err) => {
                let msg = err.to_string();
                msg.contains("Failed to build AgenticScheduler")
                    || msg.contains("Failed to start compaction sample")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sampler trait
// ---------------------------------------------------------------------------

/// Interface for the LLM call that produces compaction summaries.
///
/// Used by both intra-compaction (steps/history) and inter-compaction.
/// Implemented by each harness's sampler adapter; echo-agent-build wires its own
/// transport.
///
/// Returns [`LlmCompactionOutput`] containing both response and thinking
/// channel text. Intra-compaction uses only `.response`; inter-compaction
/// also persists `.thinking` for audit/debug.
#[async_trait]
pub trait CompactionSampler: Send + Sync {
    /// The harness's conversation item type.
    type Item;

    /// Run an LLM compaction call on the given items.
    ///
    /// Implementations should:
    /// - Build a synthetic conversation from the items + prompt.
    /// - Honor the `timeout`.
    /// - Collect both response and thinking channel text.
    async fn sample_compaction(
        &self,
        turns: &[Self::Item],
        prompt: &CompactionPrompt,
        timeout: Duration,
    ) -> Result<LlmCompactionOutput, CompactionSampleError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the inter-compaction retry classification for every variant —
    /// `Start` is intentionally deterministic here (no inter retry) even
    /// though the intra orchestrator retries its `SamplerStart` mapping.
    /// See the doc on [`CompactionSampleError::Start`] before "fixing" this.
    #[test]
    fn is_deterministic_classification() {
        assert!(
            !CompactionSampleError::Timeout {
                timeout_secs: 1,
                collected_bytes: 0
            }
            .is_deterministic()
        );
        assert!(!CompactionSampleError::EmptyResponse.is_deterministic());
        assert!(CompactionSampleError::Build("bad config".into()).is_deterministic());
        assert!(CompactionSampleError::Start("no stream".into()).is_deterministic());
        // Legacy string-matching fallback.
        assert!(
            CompactionSampleError::Other(anyhow::anyhow!(
                "Failed to build AgenticScheduler: config error"
            ))
            .is_deterministic()
        );
        assert!(
            CompactionSampleError::Other(anyhow::anyhow!(
                "Failed to start compaction sample: stream error"
            ))
            .is_deterministic()
        );
        assert!(
            !CompactionSampleError::Other(anyhow::anyhow!("transient stream error"))
                .is_deterministic()
        );
    }

    /// The `EmptyResponse` Display must keep the "no response channel
    /// content" literal the intra `Other(_)` fallback string-matches.
    #[test]
    fn empty_response_display_keeps_match_literal() {
        let msg = CompactionSampleError::EmptyResponse.to_string();
        assert!(msg.contains("no response channel content"), "got: {msg}");
    }
}
