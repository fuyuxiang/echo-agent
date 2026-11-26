//! Query handlers for the ChatStateActor.

use super::ChatStateActor;
use crate::compaction_utils::extract_last_user_query;
use crate::events::ChatStateEvent;
use crate::types::{AutoCompactTrigger, ChatStateSnapshot, ConversationCounts, NotificationMeta};

impl ChatStateActor {
    /// Build a notification meta from current timing state.
    pub(super) fn get_notification_meta(&self) -> NotificationMeta {
        NotificationMeta {
            stream_start_ms: self.state.stream_start_ms,
            turn_start_ms: self.state.turn_start_ms,
        }
    }

    /// Take a full snapshot of the actor's state.
    pub(super) fn snapshot(&self) -> ChatStateSnapshot {
        ChatStateSnapshot {
            conversation: self.state.conversation.clone(),
            sampling_config: self.state.sampling_config.clone(),
            prompt_index: self.state.prompt_index,
            total_tokens: self.state.total_tokens,
            estimate_at_last_response: self.state.estimate_at_last_response,
            agent_edited_paths: self.state.agent_edited_paths.clone(),
            prompt_texts: self.state.prompt_texts.clone(),
            stream_start_ms: self.state.stream_start_ms,
            turn_start_ms: self.state.turn_start_ms,
            last_compaction_prompt_index: self.state.last_compaction_prompt_index,
            credentials: self.state.credentials.clone(),
        }
    }

    /// Truncate conversation to a target prompt index (rewind).
    ///
    /// Walks the conversation to find the Nth `User` item (where N =
    /// `target_prompt_index`), truncates everything from that point onward,
    /// truncates `prompt_texts` to match, persists, and emits `ConversationReset`.
    ///
    /// Prompt index semantics:
    /// - 0 = no user turns have started (only system message, if any)
    /// - 1 = one user turn completed
    /// - N = N user turns completed
    ///
    /// Truncating to `target_prompt_index = 1` keeps only items up to (but not
    /// including) the 2nd `User` message.
    pub(super) fn truncate_to_prompt_index(&mut self, target_prompt_index: usize) {
        if target_prompt_index >= self.state.prompt_index {
            // Nothing to truncate — already at or before the target.
            return;
        }

        // Find the conversation position of the Nth User item.
        // Items before that position are kept; from that position onward removed.
        let mut user_count = 0;
        let mut truncate_at = self.state.conversation.len();

        for (i, item) in self.state.conversation.iter().enumerate() {
            if matches!(item, echo_agent_sampling_types::ConversationItem::User(_)) {
                if user_count == target_prompt_index {
                    truncate_at = i;
                    break;
                }
                user_count += 1;
            }
        }

        self.state.conversation.truncate(truncate_at);
        self.state.prompt_texts.truncate(target_prompt_index);
        self.state.prompt_index = target_prompt_index;
        self.state.total_tokens =
            super::state::estimate_conversation_tokens(&self.state.conversation);
        self.state.estimated_tokens_since_model = 0;
        self.state.estimate_at_last_response = self.state.total_tokens;

        self.persistence.replace_history(&self.state.conversation);

        self.send_event(ChatStateEvent::ConversationReset {
            new_len: self.state.conversation.len(),
        });
    }

    /// Check if auto-compact is needed based on token utilization.
    ///
    /// Returns `Some(AutoCompactTrigger)` if `total_tokens` exceeds
    /// `context_window * threshold_percent / 100`, otherwise `None`.
    pub(super) fn check_auto_compact_needed(
        &self,
        threshold_percent: u8,
    ) -> Option<AutoCompactTrigger> {
        let context_window = self.state.sampling_config.context_window;
        let cw = context_window.get();

        if echo_agent_token_estimation::exceeds_threshold(
            self.state.total_tokens,
            cw,
            threshold_percent,
        ) {
            let utilization_percent = echo_agent_token_estimation::usage_percentage_truncated_u8(
                self.state.total_tokens,
                cw,
            );
            Some(AutoCompactTrigger {
                total_tokens: self.state.total_tokens,
                context_window,
                utilization_percent,
            })
        } else {
            None
        }
    }

    pub(super) fn get_last_model_metadata(&self) -> crate::commands::ModelMetadata {
        self.state
            .conversation
            .iter()
            .rev()
            .find_map(|item| {
                if let echo_agent_sampling_types::ConversationItem::Assistant(a) = item {
                    Some(crate::commands::ModelMetadata {
                        resolved_model_id: a.model_id.clone(),
                        model_fingerprint: a.model_fingerprint.clone(),
                    })
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    // ─── Narrow targeted queries ─────────────────────────────────────────────

    /// Return the number of items in the conversation.
    pub(super) fn get_conversation_len(&self) -> usize {
        self.state.conversation.len()
    }

    /// Whether the conversation has any assistant tool call without a matching
    /// `ToolResult` (the dangling-tool-call repair would fire on the next build).
    pub(super) fn has_dangling_tool_calls(&self) -> bool {
        echo_agent_sampling_types::has_dangling_tool_calls(&self.state.conversation)
    }

    /// Return the text content of the last assistant message with non-empty text.
    ///
    /// Walks the conversation backwards and returns the first `Assistant` item
    /// whose `content` field is non-empty after trimming. Returns `None` when
    /// no such item exists.
    pub(super) fn get_last_assistant_text(&self) -> Option<String> {
        self.state.conversation.iter().rev().find_map(|item| {
            if let echo_agent_sampling_types::ConversationItem::Assistant(a) = item
                && !a.content.trim().is_empty()
            {
                return Some(a.content.as_ref().to_owned());
            }
            None
        })
    }

    /// Return the current turn's last assistant message with non-empty text, or
    /// `None` when the turn produced none.
    ///
    /// Like [`Self::get_last_assistant_text`], but the backwards walk stops at the
    /// turn boundary (a user item with `prompt_index` set, a genuine user message,
    /// or a synthetic reason with [`SyntheticReason::starts_prompt_turn`]); mid-turn
    /// synthetic injections are walked past.
    ///
    /// [`SyntheticReason::starts_prompt_turn`]: echo_agent_sampling_types::SyntheticReason::starts_prompt_turn
    pub(super) fn get_last_assistant_text_in_turn(&self) -> Option<String> {
        for item in self.state.conversation.iter().rev() {
            match item {
                echo_agent_sampling_types::ConversationItem::Assistant(a)
                    if !a.content.trim().is_empty() =>
                {
                    return Some(a.content.as_ref().to_owned());
                }
                echo_agent_sampling_types::ConversationItem::User(u)
                    if u.prompt_index.is_some()
                        || u.synthetic_reason
                            .as_ref()
                            .is_none_or(|r| r.starts_prompt_turn()) =>
                {
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    /// Return the text of the **first content part** of the first `User` message,
    /// if and only if that part is `ContentPart::Text`.
    ///
    /// Matches the original call-site semantics exactly: if the first user
    /// message leads with a non-text part (e.g. an image in a multimodal
    /// prompt), this returns `None` rather than scanning further parts.
    /// Callers that need "any text part" rather than "first-part-is-text"
    /// should use `get_conversation()` directly.
    pub(super) fn get_first_user_text(&self) -> Option<String> {
        self.state.conversation.iter().find_map(|item| {
            if let echo_agent_sampling_types::ConversationItem::User(u) = item {
                // Only return text if the first part is Text — behaviour-preserving
                // w.r.t. the original `content.first().and_then(|p| if Text { … })`.
                u.content.first().and_then(|part| {
                    if let echo_agent_sampling_types::ContentPart::Text { text } = part {
                        Some(text.as_ref().to_owned())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        })
    }

    /// Return the conversation item at `index`, or `None` if out of bounds.
    pub(super) fn get_conversation_item_at(
        &self,
        index: usize,
    ) -> Option<echo_agent_sampling_types::ConversationItem> {
        self.state.conversation.get(index).cloned()
    }

    /// Return the processed text of the last user query (metadata tags stripped).
    ///
    /// Delegates to [`extract_last_user_query`] so the caller does not need a
    /// full conversation clone.
    pub(super) fn get_last_user_query_text(&self) -> Option<String> {
        extract_last_user_query(&self.state.conversation)
    }

    /// Return conversation item counts by role without cloning any items.
    pub(super) fn get_conversation_counts(&self) -> ConversationCounts {
        let mut counts = ConversationCounts {
            total: self.state.conversation.len(),
            ..Default::default()
        };
        for item in &self.state.conversation {
            match item {
                echo_agent_sampling_types::ConversationItem::User(_) => counts.user += 1,
                echo_agent_sampling_types::ConversationItem::Assistant(_) => {
                    counts.assistant += 1;
                }
                echo_agent_sampling_types::ConversationItem::ToolResult(_) => {
                    counts.tool_result += 1;
                }
                echo_agent_sampling_types::ConversationItem::System(_) => {}
                echo_agent_sampling_types::ConversationItem::BackendToolCall(_) => {}
                echo_agent_sampling_types::ConversationItem::Reasoning(_) => {}
            }
        }
        counts
    }

    /// Return the first `System` message in the conversation, or `None`.
    pub(super) fn get_system_message(&self) -> Option<echo_agent_sampling_types::ConversationItem> {
        self.state
            .conversation
            .iter()
            .find(|item| matches!(item, echo_agent_sampling_types::ConversationItem::System(_)))
            .cloned()
    }
}
