//! ConversationRequest assembly — image compaction, pruning, repair, memory injection.

use echo_agent_sampling_types::{ConversationItem, ConversationRequest, ToolSpec, TraceContext};

use super::ChatStateActor;
use crate::events::ChatStateEvent;
use crate::image_budget::{ImageBudgetOutcome, apply_image_budget};
use crate::types::PruningConfig;

/// Placeholder inserted when a tool result is hard-cleared.
///
/// `pub(super)` so that `mutations.rs` can use the same string when it
/// hard-clears tool results in the retained in-memory conversation.
pub(super) const HARD_CLEAR_PLACEHOLDER: &str = "[Tool result omitted — too old]";

/// Separator inserted between head and tail in soft-trimmed results.
const SOFT_TRIM_SEPARATOR: &str = "\n\n[…trimmed…]\n\n";

impl ChatStateActor {
    /// Build a `ConversationRequest` from the current actor state.
    ///
    /// 1. Evict oldest inline images when the inline-image bytes near 50 MB
    /// 2. Prune old tool results if over 50% context utilization
    /// 3. Optionally persist the memory reminder into actor state
    /// 4. Inject memory reminder into the request clone (if needed)
    /// 5. Assemble and return the `ConversationRequest`
    ///
    /// # Repair invariant
    ///
    /// The `BuildConversationRequest` command handler calls
    /// `ensure_conversation_integrity()` on the actor's own conversation
    /// **before** this function runs. The clone therefore starts from an
    /// already-repaired state, so there is no need to run
    /// `dedup_duplicate_tool_results` / `repair_dangling_tool_calls` on the
    /// clone — those would be O(n) no-ops.
    pub(super) fn build_conversation_request(
        &mut self,
        tool_definitions: Vec<ToolSpec>,
        memory_reminder: Option<String>,
        persist_memory_reminder: bool,
        trace: Option<Box<dyn TraceContext>>,
        conv_id: String,
        req_id: String,
    ) -> ConversationRequest {
        let needs_prune = should_prune(
            self.state.total_tokens,
            self.state.sampling_config.context_window,
        );
        let mut memory_reminder = memory_reminder;
        if let Some(reminder) = memory_reminder.as_deref()
            && persist_memory_reminder
        {
            // A live in-place inject can prepend a `System` item, shifting indices
            // under an active capture; snapshot + rebase like the other mutators.
            self.snapshot_turn_slice();
            let injected = inject_memory_reminder(&mut self.state.conversation, reminder);
            if injected {
                self.persistence.replace_history(&self.state.conversation);
                memory_reminder = None;
            }
            self.rebase_turn_capture_offset();
        }
        let budgeted = apply_image_budget(self.state.conversation.clone());
        let ImageBudgetOutcome {
            body_bytes,
            body_bytes_after,
            inline_images,
            needs_image_compaction,
            evicted,
        } = budgeted.outcome;
        let mut items = budgeted.items;
        if inline_images > 0 {
            self.send_event(ChatStateEvent::ImageBudget {
                body_bytes,
                trigger_bytes: crate::image_budget::IMAGE_COMPACT_TRIGGER_BYTES,
                reclaim_target_bytes: crate::image_budget::IMAGE_COMPACT_RECLAIM_TARGET_BYTES,
                inline_images,
                needs_image_compaction,
                evicted,
                body_bytes_after,
            });
        }
        if needs_prune {
            prune_conversation(&mut items, &self.pruning_config);
        }
        if !persist_memory_reminder {
            if let Some(ConversationItem::System(system)) = items.first_mut() {
                if let Some(start) = system.content.find(MEMORY_CONTEXT_OPEN_TAG) {
                    let end = system.content[start..]
                        .find(crate::types::MEMORY_CONTEXT_CLOSE_TAG)
                        .map(|i| start + i + crate::types::MEMORY_CONTEXT_CLOSE_TAG.len())
                        .unwrap_or(system.content.len());
                    system.content =
                        format!("{}{}", &system.content[..start], &system.content[end..]).into();
                }
            }
        }
        if let Some(reminder) = memory_reminder {
            inject_memory_reminder(&mut items, &reminder);
        }
        items = crate::compaction_utils::ModelRequestHistory::from_raw(items).into_items();

        // Step 4: Assemble request
        ConversationRequest {
            preflight: None,
            items,
            tools: tool_definitions,
            hosted_tools: vec![],
            tool_choice: None,
            model: Some(self.state.sampling_config.model.clone()),
            temperature: self.state.sampling_config.temperature,
            max_output_tokens: self.state.sampling_config.max_completion_tokens,
            top_p: self.state.sampling_config.top_p,
            x_echo_agent_conv_id: Some(conv_id),
            x_echo_agent_req_id: Some(req_id),
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace,
            prompt_cache_key: None,
            reasoning_effort: self.state.sampling_config.reasoning_effort,
            json_schema: None,
        }
    }
}

// ============================================================================
// Pruning (standalone functions, no actor state needed)
// ============================================================================

/// Check whether pruning should run based on context utilization.
///
/// Returns `true` when `total_tokens` exceeds 50% of `context_window`.
pub(crate) fn should_prune(total_tokens: u64, context_window: std::num::NonZeroU64) -> bool {
    total_tokens > context_window.get() / 2
}

/// Prune old, large tool results from the conversation in place.
///
/// Only prompt boundaries age the preceding results. Reminders and
/// interjections belong to their admitting turn.
pub(crate) fn prune_conversation(conversation: &mut [ConversationItem], config: &PruningConfig) {
    if !config.enabled {
        return;
    }

    let mut turn_from_end: usize = 0;

    for i in (0..conversation.len()).rev() {
        if let ConversationItem::User(user) = &conversation[i] {
            if user.prompt_index.is_some()
                || user
                    .synthetic_reason
                    .as_ref()
                    .is_some_and(|reason| reason.starts_prompt_turn())
                || crate::compaction_utils::is_real_user_turn(&conversation[i])
            {
                turn_from_end += 1;
            }
            continue;
        }

        let ConversationItem::ToolResult(tool_result) = &mut conversation[i] else {
            continue;
        };

        // Never prune recent turns.
        if turn_from_end < config.keep_last_n_turns {
            continue;
        }

        // Hard clear: very old tool results → replace entirely.
        if turn_from_end >= config.hard_clear_age_turns {
            if tool_result.content.as_ref() != HARD_CLEAR_PLACEHOLDER {
                tool_result.content = std::sync::Arc::<str>::from(HARD_CLEAR_PLACEHOLDER);
            }
            continue;
        }

        // Soft trim: large tool results → keep head + tail.
        let content_len = tool_result.content.chars().count();
        if content_len > config.soft_trim_threshold {
            let head = safe_char_slice(&tool_result.content, 0, config.soft_trim_head);
            let tail = safe_char_slice_tail(&tool_result.content, config.soft_trim_tail);
            tool_result.content =
                std::sync::Arc::<str>::from(format!("{head}{SOFT_TRIM_SEPARATOR}{tail}"));
        }
    }
}

// ============================================================================
// Memory reminder injection
// ============================================================================

use crate::types::MEMORY_CONTEXT_OPEN_TAG;

/// Upsert a memory reminder into the conversation's system message.
///
/// If the first item is a `System` message, any previously injected memory
/// reminder section is replaced in-place; otherwise the reminder is appended.
/// If no system message exists, a new `System` item is prepended.
///
/// Returns `true` when the conversation was changed.
pub(super) fn inject_memory_reminder(items: &mut Vec<ConversationItem>, reminder: &str) -> bool {
    let reminder = reminder.trim();
    if reminder.is_empty() {
        return false;
    }

    if let Some(ConversationItem::System(sys)) = items.first_mut() {
        upsert_memory_reminder_text(&mut sys.content, reminder)
    } else {
        items.insert(0, ConversationItem::system(reminder));
        true
    }
}

fn upsert_memory_reminder_text(system_prompt: &mut std::sync::Arc<str>, reminder: &str) -> bool {
    let existing_start = system_prompt
        .find(MEMORY_CONTEXT_OPEN_TAG)
        .map(|idx| system_prompt[..idx].trim_end_matches('\n').len());

    let updated: String = if let Some(prefix_len) = existing_start {
        let prefix = system_prompt[..prefix_len].trim_end_matches('\n');
        if prefix.is_empty() {
            reminder.to_string()
        } else {
            format!("{prefix}\n\n{reminder}")
        }
    } else if system_prompt.trim_end() == reminder {
        system_prompt.as_ref().to_owned()
    } else if system_prompt.is_empty() {
        reminder.to_string()
    } else {
        format!("{}\n\n{reminder}", system_prompt.trim_end_matches('\n'))
    };

    if system_prompt.as_ref() == updated.as_str() {
        false
    } else {
        *system_prompt = std::sync::Arc::<str>::from(updated);
        true
    }
}

// ============================================================================
// String helpers
// ============================================================================

fn safe_char_slice(s: &str, start: usize, count: usize) -> String {
    s.chars().skip(start).take(count).collect()
}

fn safe_char_slice_tail(s: &str, count: usize) -> String {
    let total = s.chars().count();
    if count >= total {
        return s.to_string();
    }
    s.chars().skip(total - count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_prune_gating() {
        use std::num::NonZeroU64;
        let cw = NonZeroU64::new(10000).unwrap();
        assert!(!should_prune(1000, cw)); // 10%
        assert!(should_prune(6000, cw)); // 60%
        assert!(!should_prune(5000, cw)); // 50% exact (> not >=)
    }

    #[test]
    fn prune_disabled_is_noop() {
        let mut conv = vec![ConversationItem::tool_result("c1", "x".repeat(10_000))];
        let config = PruningConfig {
            enabled: false,
            ..Default::default()
        };
        prune_conversation(&mut conv, &config);
        if let ConversationItem::ToolResult(ref tr) = conv[0] {
            assert_eq!(tr.content.len(), 10_000);
        }
    }

    #[test]
    fn pruning_protects_recent_tools_across_mid_turn_reminders() {
        let old = "old tool result".repeat(1000);
        let recent = "current tool result".repeat(1000);
        let mut conversation = vec![
            ConversationItem::user("old user prompt"),
            ConversationItem::tool_result("old", old),
            ConversationItem::user("current user prompt"),
            ConversationItem::tool_result("current", recent.clone()),
            ConversationItem::system_reminder("runtime reminder one"),
            ConversationItem::system_reminder("runtime reminder two"),
        ];
        prune_conversation(
            &mut conversation,
            &PruningConfig {
                enabled: true,
                keep_last_n_turns: 1,
                hard_clear_age_turns: 1,
                ..Default::default()
            },
        );
        let ConversationItem::ToolResult(old) = &conversation[1] else {
            panic!("expected tool result");
        };
        let ConversationItem::ToolResult(current) = &conversation[3] else {
            panic!("expected tool result");
        };
        assert_eq!(old.content.as_ref(), HARD_CLEAR_PLACEHOLDER);
        assert_eq!(current.content.as_ref(), recent);
    }
    #[test]
    fn inject_memory_into_existing_system() {
        let mut items = vec![
            ConversationItem::system("You are helpful."),
            ConversationItem::user("hi"),
        ];
        inject_memory_reminder(&mut items, "Remember: user likes rust");
        if let ConversationItem::System(ref sys) = items[0] {
            assert!(sys.content.contains("Remember: user likes rust"));
            assert!(sys.content.starts_with("You are helpful."));
        }
        assert_eq!(items.len(), 2); // no new item added
    }

    #[test]
    fn inject_memory_prepends_when_no_system() {
        let mut items = vec![ConversationItem::user("hi")];
        inject_memory_reminder(&mut items, "Remember: user likes rust");
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], ConversationItem::System(_)));
    }
}
