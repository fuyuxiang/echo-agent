//! Conversation history compaction — shared selection/assembly logic for compacting
//! prior conversation turns into a summary.
//!
//! Everything here is generic over [`CompactionItem`](crate::CompactionItem)
//! / [`CompactionItemBuilder`](crate::CompactionItemBuilder) or pure
//! string/text manipulation. Harness-bound extraction (EchoAgent chat's
//! `EchoAgentConversation` traversal, `ChatCompletionRequest` user-message
//! extraction, `EchoAgentMessage` assembly) stays in the harness crate.

pub mod filter;
pub mod prompt;
pub mod types;
pub mod validate;

pub use filter::{
    SeparatedHistoryTurns, assemble_user_queries_preamble, build_user_queries_preamble,
    extract_prior_user_queries, extract_user_queries_from_turns, filter_turns_for_basic,
    filter_turns_for_inter_compaction, keep_turn_for_basic_compaction, separate_prior_user_queries,
    split_prior_compaction_text, truncate_middle, wrap_chunk_analysis,
};
pub use prompt::{format_compaction_developer_prompt, format_compaction_user_prompt};
pub use types::CompactionStrategy;
pub use validate::{CompactionValidationError, validate_compaction_text};
