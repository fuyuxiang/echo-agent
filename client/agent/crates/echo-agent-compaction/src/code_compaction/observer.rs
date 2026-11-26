//! Observability seam for the full-replace (echo-agent-build) pass.
//!
//! The shared orchestrator reports per-attempt and terminal outcomes through
//! this trait so each harness can emit its own telemetry (echo-agent-build:
//! `CompactionAttempt` rows, `CompactionRetryDegraded` events, span records,
//! request-artifact persistence) without the shared crate depending on a
//! telemetry backend. Mirrors
//! [`IntraCompactionObserver`](crate::intra_compaction::IntraCompactionObserver)
//! / [`InterCompactionObserver`](crate::inter_compaction::InterCompactionObserver).
//!
//! Emission points are part of the behavior contract: the echo-agent-build observer
//! preserves the pre-migration `CompactionAttempt`/`CompactionRetryDegraded`
//! semantics byte-for-byte.

use std::time::Duration;

/// Classified outcome of a single full-replace sample attempt.
///
/// The harness turns this into its per-attempt telemetry row. `summary` is the
/// raw model output (the harness bounds/captures it as needed); it is borrowed
/// for the duration of the callback so no allocation happens on the hot path.
#[derive(Debug)]
pub enum FullReplaceAttemptOutcome<'a> {
    /// A usable, non-degenerate summary was produced; the pass will succeed.
    Success {
        /// Raw model summary text.
        summary: &'a str,
    },
    /// The model returned an empty / whitespace-only response.
    EmptyResponse {
        /// Whether the orchestrator will retry after this attempt.
        will_retry: bool,
    },
    /// The cleaned summary seed was too short to carry the conversation's task
    /// state; retried like a transient failure.
    Degenerate {
        /// Raw model summary text (still captured for offline inspection).
        summary: &'a str,
        /// Whether the orchestrator will retry after this attempt.
        will_retry: bool,
    },
    /// The sampler returned an error.
    Failure {
        /// Rendered error message.
        message: &'a str,
        /// Whether re-sending the *same* input cannot help (auth / schema /
        /// size). Transient failures (timeout / stream blip / 5xx) are `false`.
        deterministic: bool,
        /// Whether the failure was a context-length overflow — the signal the
        /// harness uses to step its input ladder rather than suppress.
        context_overflow: bool,
        /// Whether the orchestrator will retry after this attempt (always
        /// `false` for deterministic failures and context overflows).
        will_retry: bool,
    },
}

/// Receives full-replace compaction outcomes. All methods default to no-ops so
/// harnesses without telemetry (and tests) can use `()`.
pub trait FullReplaceObserver: Send + Sync {
    /// One sample attempt finished with the given classified outcome.
    /// `attempt` is 1-based and cumulative across the pass.
    fn on_attempt(&self, _attempt: u32, _outcome: &FullReplaceAttemptOutcome<'_>) {}

    /// The pass succeeded after `attempts` total attempts.
    fn on_success(&self, _attempts: u32, _summary_chars: usize, _elapsed: Duration) {}

    /// The pass failed terminally after `attempts` total attempts.
    fn on_error(&self, _attempts: u32) {}
}

/// No-op observer for tests and harnesses without telemetry.
impl FullReplaceObserver for () {}
