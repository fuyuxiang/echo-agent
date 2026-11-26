//! Conversation types: re-exports the canonical set from
//! `echo_agent_sampling_types` plus echo-agent-shell-specific additions.

use std::collections::HashSet;

pub use echo_agent_sampling_types::conversation::*;

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;

/// Tracing context for conversation requests; satisfies `TraceContext`
/// through its blanket impl. Lives in echo-agent-shell because it references
/// shell-internal config and upload types.
#[derive(Debug, Clone)]
pub struct ConversationRequestTrace {
    pub gcs_config: crate::session::repo_changes::TraceExportConfig,
    #[expect(
        dead_code,
        reason = "retained for snapshot compat; wire when sampler path uploads traces"
    )]
    pub(crate) artifact_tracker: Option<crate::upload::manifest::ArtifactTracker>,
}

/// Fork-safety filter for copied chat history: drops synthetic user messages,
/// then truncates at the last complete turn so the child never sees a partial
/// one. A turn is complete when the Assistant's tool calls are all answered;
/// Reasoning and BackendToolCall items are transparent to the scan.
///
/// NOTE: keep the "complete turn" definition in sync with
/// `count_complete_turns` in `echo-agent-subagent-resolution/src/context.rs`.
pub(crate) fn fork_filter_chat(items: &mut Vec<ConversationItem>) {
    items.retain(|item| match item {
        ConversationItem::User(u) => u.synthetic_reason.is_none(),
        _ => true,
    });

    // Only Assistant advances the boundary; everything else is transparent.
    let mut last_complete_end = 0;
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            ConversationItem::System(_) => {
                last_complete_end = i + 1;
                i += 1;
            }
            ConversationItem::Assistant(asst) => {
                let expected: HashSet<&str> =
                    asst.tool_calls.iter().map(|tc| tc.id.as_ref()).collect();
                let mut found = HashSet::new();
                let mut j = i + 1;
                while j < items.len() {
                    match &items[j] {
                        ConversationItem::ToolResult(tr) => {
                            if expected.contains(tr.tool_call_id.as_str()) {
                                found.insert(tr.tool_call_id.as_str());
                            }
                            j += 1;
                        }
                        ConversationItem::Reasoning(_) | ConversationItem::BackendToolCall(_) => {
                            j += 1;
                        }
                        _ => break,
                    }
                }
                if found == expected {
                    last_complete_end = j;
                    i = j;
                } else {
                    break; // dangling tool calls -> stop at the last complete boundary
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    items.truncate(last_complete_end);
}
