//! Shared prompt-queue wire types and combine-queued-prompts merge rules.

mod combine;
mod types;

pub use combine::{
    CombineGate, TEXT_SEPARATOR, can_merge_follower, can_merge_front, combine_prefix_len,
    is_combined, join_texts, stamp_combined_display_texts,
};
pub use types::{COMBINED_DISPLAY_TEXTS_META, QueueChanged, QueueEntryMeta, QueueEntryWire};
