//! Shared, transport-agnostic compaction engine.
//!
//! This crate is the `compaction-core`: shared policy, prompts, selection,
//! and assembly. Host-specific trigger wiring, transport, persistence /
//! replay / rewind, state commit, metrics backends, and prompt-variant forks
//! stay in each product host (for example `echo-agent-runtime`).
//!
//! The crate depends on **neither** a conversation-type crate nor
//! `echo-agent-sampling-types`. It is decoupled from both EchoAgent chat and
//! echo-agent-build hosts through a small set of trait seams:
//!
//! - [`CompactionItem`] / [`CompactionRole`] / [`CompactionItemBuilder`] —
//!   abstracts a single turn and its reconstruction.
//! - [`ItemTokenCounter`] — trusted token counting per host.
//! - [`CompactionSampler`] — the LLM call.
//! - [`CompactionStreamProc`](intra_compaction::CompactionStreamProc) —
//!   state commit for intra-compaction.
//! - [`IntraCompactionObserver`](intra_compaction::IntraCompactionObserver) /
//!   [`InterCompactionObserver`](inter_compaction::InterCompactionObserver)
//!   — host metrics.
//!
//! Compaction styles live in their own modules:
//!
//! - [`code_compaction`] — echo-agent-build's whole-session **full-replace**
//!   subsystem (prompt/summary/failure/config, assemble, orchestration).
//! - [`intra_compaction`] — EchoAgent chat's tail-keep, per-step pass.
//! - [`inter_compaction`] — EchoAgent chat's chunked, between-turn pass.
//!
//! Compaction-type content (parallel subfolders): [`steps`] (the step-level
//! prompt) and [`history`] (filtering, history prompts, validation +
//! user-query preservation).
//!
//! Shared seams/primitives: [`item`], [`token`], [`sampler`],
//! [`prompt::CompactionPrompt`], [`select`] (tool-pair-safe tail-keep
//! selection — shared by the intra `Steps` and `History` targets, so it stays
//! neutral at the crate root rather than under `steps`), and [`reminder`]
//! (active-agent-state `<system-reminder>` formatting shared by EchoAgent chat and
//! echo-agent-build; hosts still own snapshotting and host-only sections).

pub mod code_compaction;
pub mod history;
pub mod inter_compaction;
pub mod intra_compaction;
pub mod item;
pub mod prompt;
pub mod reminder;
pub mod sampler;
pub mod select;
pub mod steps;
pub mod token;

/// Shared code default for the dedicated compaction model name.
///
/// Override order (highest first):
/// 1. the agent's `compaction_model_name` setting (non-blank)
/// 2. service / harness config YAML
/// 3. this constant
pub use intra_compaction::DEFAULT_COMPACTION_MODEL_NAME;

// echo-agent-build's full-replace subsystem now lives under `code_compaction`;
// re-exported at the crate root so consumers keep a stable public API.
pub use code_compaction::{
    CompactedHistoryParts, DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT, FailureKind,
    FullReplaceAttemptOutcome, FullReplaceConfig, FullReplaceContext, FullReplaceError,
    FullReplaceObserver, FullReplaceOutput, FullReplaceSummary, MIN_SUMMARY_SEED_CHARS,
    SELF_SUMMARIZATION_PROMPT, SummaryPromptKind, apply_full_replace_compaction,
    assemble_compacted_history, build_summary_prompt, build_summary_prompt_kind,
    classify_http_status, classify_stream_event_error, format_compact_summary,
    format_compact_summary_content, is_context_length_error, is_degenerate_summary,
    sample_full_replace_summary, wrap_user_query,
};
pub use item::{
    CompactionFileRef, CompactionItem, CompactionItemBuilder, CompactionItemFactory, CompactionRole,
};
pub use prompt::CompactionPrompt;
// Reminder types/formatters: import from `reminder::` (borrowed views).
// Only the summary-injection helper is re-exported at the crate root — both
// intra FullReplace and inter already use it by this name.
pub use reminder::append_reminder_block;
pub use sampler::{CompactionSampleError, CompactionSampler, LlmCompactionOutput};
pub use select::{SplitPlan, select_turns_to_compact};
pub use steps::format_compaction_prompt;
pub use token::ItemTokenCounter;
