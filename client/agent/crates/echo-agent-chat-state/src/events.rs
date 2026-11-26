//! Events emitted by the ChatStateActor.

/// Events emitted by the ChatStateActor to the session main loop.
///
/// Persistence is handled internally by the actor — these events are for
/// session-level coordination only.
#[derive(Debug, Clone)]
pub enum ChatStateEvent {
    /// Prompt index changed (session uses this to update hunk tracker attribution).
    PromptIndexChanged { new_index: usize },

    /// Token count updated (session uses this for notification meta,
    /// auto-compact threshold checks).
    TokensUpdated { total_tokens: u64 },

    /// Conversation was replaced (compaction/rewind) — session may need to
    /// reset idle-flush counters, memory injection flags, etc.
    ConversationReset { new_len: usize },

    /// Image byte-budget record for a built request (observability only,
    /// emitted on image-bearing turns). The session consumer writes this to
    /// the local unified log for verification. `evicted == 0` means the body
    /// was under the trigger and every image was kept.
    ImageBudget {
        /// Exact serialized conversation body size measured for the gate.
        body_bytes: usize,
        /// Threshold at which eviction fires.
        trigger_bytes: usize,
        /// Low-water mark eviction reclaims down to once it fires.
        reclaim_target_bytes: usize,
        /// Inline images present before eviction.
        inline_images: usize,
        /// Whether the body crossed the trigger this turn.
        needs_image_compaction: bool,
        /// Images replaced with a placeholder this turn.
        evicted: usize,
        /// Estimated body size after eviction (== `body_bytes` when none).
        body_bytes_after: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_variants_are_constructible() {
        let _ = ChatStateEvent::PromptIndexChanged { new_index: 1 };
        let _ = ChatStateEvent::TokensUpdated { total_tokens: 500 };
        let _ = ChatStateEvent::ConversationReset { new_len: 3 };
    }
}
