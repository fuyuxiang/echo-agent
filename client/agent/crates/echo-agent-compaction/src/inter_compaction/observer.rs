//! Observability seam for inter-compaction.
//!
//! Same rationale as [`crate::intra_compaction::observer`]: the shared
//! pipeline reports events; each harness emits its own metrics. Emission
//! points and label values are part of the behavior contract.

use std::time::Duration;

/// Receives inter-compaction pipeline events. All methods default to no-ops.
pub trait InterCompactionObserver: Send + Sync {
    /// A prior compaction summary was found in the input (re-compaction).
    /// `strategy` is the stable label from `CompactionStrategy::label()`.
    fn on_recompaction(&self, _strategy: &'static str) {}

    /// One chunk's LLM call finished (success or error).
    fn on_chunk_sampled(&self, _success: bool, _elapsed: Duration) {}

    /// The whole pipeline finished assembling `num_chunks` chunk summaries.
    fn on_chunk_count(&self, _num_chunks: usize) {}
}

/// No-op observer for tests and harnesses without metrics.
impl InterCompactionObserver for () {}
