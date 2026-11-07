//! Pure utility functions and types for compaction support.
//!
//! These are stateless functions that operate on conversation data only —
//! no I/O, no actor state. They live in `echo-agent-chat-state` so that both
//! this crate and `echo-agent-runtime` can share them without duplication.
use echo_agent_sampling_types::{ContentPart, ConversationItem, SyntheticReason, ToolResultItem};
use std::collections::BTreeSet;
pub const AGENT_MESSAGE_MODEL_LABEL: &str =
    "[Message authored by another agent; not a human request or approval.]";
/// Canonical history prepared exactly once for a model-facing request.
///
/// The private inner value distinguishes prepared history without inspecting
/// or rewriting payload text.
pub struct ModelRequestHistory(Vec<ConversationItem>);
impl ModelRequestHistory {
    pub fn from_raw(conversation: Vec<ConversationItem>) -> Self {
        Self(
            conversation
                .into_iter()
                .map(|item| match item {
                    ConversationItem::User(mut user)
                        if user.synthetic_reason == Some(SyntheticReason::AgentMessage) =>
                    {
                        user.content.insert(
                            0,
                            ContentPart::Text {
                                text: std::sync::Arc::<str>::from(AGENT_MESSAGE_MODEL_LABEL),
                            },
                        );
                        ConversationItem::User(user)
                    }
                    other => other,
                })
                .collect(),
        )
    }
    pub fn into_items(self) -> Vec<ConversationItem> {
        self.0
    }
}
/// Drops tool results and backend tool calls, and flattens assistant
/// `tool_calls` into `[Called tools: ...]` text annotations.
///
/// Mutates assistant text in place; do NOT use this directly when sending
/// to a provider that validates signed `reasoning` blocks against the
/// surrounding content. Use [`prepare_conversation_for_summarization`]
/// instead, which also strips `reasoning` so the mutation is safe.
pub(crate) fn strip_tool_messages_for_conversation_item(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult(_) => None,
            ConversationItem::BackendToolCall(_) => None,
            ConversationItem::Assistant(mut a) => {
                if !a.tool_calls.is_empty() {
                    let tool_names: Vec<String> =
                        a.tool_calls.iter().map(|tc| tc.name.clone()).collect();
                    let tool_info = format!("\n[Called tools: {}]", tool_names.join(", "));
                    a.content = if a.content.is_empty() {
                        std::sync::Arc::<str>::from(tool_info)
                    } else {
                        let mut s = String::with_capacity(a.content.len() + tool_info.len());
                        s.push_str(&a.content);
                        s.push_str(&tool_info);
                        std::sync::Arc::<str>::from(s)
                    };
                    a.tool_calls.clear();
                }
                Some(ConversationItem::Assistant(a))
            }
            other => Some(other),
        })
        .collect()
}
/// Drops every `ConversationItem::Reasoning(_)` sibling.
///
/// Required before sending to backends that reject the structured reasoning
/// shape (signed `Thinking` blocks after text mutation; some Chat Completions
/// providers entirely) and before summarization.
pub fn strip_reasoning_blocks(conversation: Vec<ConversationItem>) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .filter(|item| !matches!(item, ConversationItem::Reasoning(_)))
        .collect()
}
/// Replace `ContentPart::Image` entries with `"[image]"` so downstream
/// consumers (summary model, segment store) don't carry megabytes of base64.
pub(crate) fn strip_images(conversation: Vec<ConversationItem>) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .map(|item| match item {
            ConversationItem::User(mut u) => {
                for part in &mut u.content {
                    if matches!(part, ContentPart::Image { .. }) {
                        *part = ContentPart::Text {
                            text: std::sync::Arc::<str>::from("[image]"),
                        };
                    }
                }
                ConversationItem::User(u)
            }
            other => other,
        })
        .collect()
}
/// Prepare a conversation for a summarization call (compaction or memory flush).
///
/// Combines `strip_tool_messages_for_conversation_item` (drops tool
/// results, flattens `tool_calls` into text annotations),
/// `strip_reasoning_blocks`, and `strip_images`.
///
/// The reasoning strip is required because the text mutation in the
/// tool-message step would invalidate signed `thinking` blocks, which
/// strict providers reject with a 400.
///
/// The image strip replaces `ContentPart::Image` with `"[image]"` so the
/// summarizer doesn't receive megabytes of base64 data.
pub fn prepare_conversation_for_summarization(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    strip_images(strip_reasoning_blocks(
        strip_tool_messages_for_conversation_item(conversation),
    ))
}
/// Segment-store prep (`segments` mode): keep tool I/O verbatim, strip only images + reasoning.
pub fn prepare_conversation_for_segment(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    strip_images(strip_reasoning_blocks(conversation))
}
/// Drop a trailing assistant turn whose `tool_calls` lack a `ToolResult` (else strict backends reject the dangling `tool_use`).
pub fn truncate_trailing_incomplete_tool_call(
    mut conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    while matches!(
        conversation.last(),
        Some(ConversationItem::Assistant(a)) if !a.tool_calls.is_empty()
    ) {
        conversation.pop();
    }
    conversation
}
/// Cache-aligned summarizer prep: keep tool I/O + images so the prefix matches the engine cache; set `strip_reasoning` when the provider rejects mutated thinking blocks.
pub fn prepare_conversation_for_verbatim_summarization(
    conversation: Vec<ConversationItem>,
    strip_reasoning: bool,
) -> Vec<ConversationItem> {
    let conversation = if strip_reasoning {
        strip_reasoning_blocks(conversation)
    } else {
        conversation
    };
    truncate_trailing_incomplete_tool_call(conversation)
}
/// Per-item token estimate via the trigger-side estimator, so `fit`'s budget matches what fired the compaction (counts images + encrypted reasoning).
fn estimate_item_tokens(item: &ConversationItem) -> u64 {
    crate::actor::state::estimate_item_tokens(item)
}
/// Shrink a verbatim conversation to `max_tokens`: drop oldest whole turns (System kept, tool runs unsplit; the last turn is truncated in place rather than dropped).
pub fn fit_conversation_to_budget(
    conversation: Vec<ConversationItem>,
    max_tokens: u64,
) -> Vec<ConversationItem> {
    let total: u64 = conversation.iter().map(estimate_item_tokens).sum();
    if total <= max_tokens {
        return conversation;
    }
    let mut head: Vec<ConversationItem> = Vec::new();
    let mut body: Vec<ConversationItem> = conversation;
    if matches!(body.first(), Some(ConversationItem::System(_))) {
        head.push(body.remove(0));
    }
    let budget = max_tokens.saturating_sub(head.iter().map(estimate_item_tokens).sum::<u64>());
    let mut remaining = budget;
    let mut start = body.len();
    for i in (0..body.len()).rev() {
        let cost = estimate_item_tokens(&body[i]);
        if cost > remaining {
            break;
        }
        remaining -= cost;
        start = i;
    }
    while start < body.len() && matches!(body[start], ConversationItem::ToolResult(_)) {
        start += 1;
    }
    if start < body.len() {
        head.extend(body.into_iter().skip(start));
    } else {
        head.extend(recover_truncated_tail_unit(body, budget));
    }
    head
}
/// Keep the most-recent turn but truncate its content to `budget` (with its owning `tool_use`) instead of dropping it.
fn recover_truncated_tail_unit(
    mut body: Vec<ConversationItem>,
    budget: u64,
) -> Vec<ConversationItem> {
    let mut results: Vec<ConversationItem> = Vec::new();
    while matches!(body.last(), Some(ConversationItem::ToolResult(_))) {
        results.push(body.pop().expect("last() was Some"));
    }
    results.reverse();
    if results.is_empty() {
        return match body.pop() {
            Some(item) => vec![truncate_item_to_tokens(item, budget)],
            None => Vec::new(),
        };
    }
    let owner = if matches!(
        body.last(),
        Some(ConversationItem::Assistant(a)) if !a.tool_calls.is_empty()
    ) {
        body.pop()
    } else {
        None
    };
    let owner_cost = owner.as_ref().map(estimate_item_tokens).unwrap_or(0);
    let result_budget = budget.saturating_sub(owner_cost);
    let per = (result_budget / results.len() as u64).max(1);
    let mut unit: Vec<ConversationItem> = Vec::new();
    if let Some(o) = owner {
        unit.push(o);
    }
    unit.extend(results.into_iter().map(|r| truncate_item_to_tokens(r, per)));
    unit
}
/// Truncate one item's content text to at most `max_tokens`, appending a `[... truncated N bytes ...]` marker (structural fields kept).
fn truncate_item_to_tokens(item: ConversationItem, max_tokens: u64) -> ConversationItem {
    let max_bytes = (max_tokens as usize).saturating_mul(4);
    match item {
        ConversationItem::ToolResult(mut t) => {
            if let Some(s) = truncate_text_to_bytes(&t.content, max_bytes) {
                t.content = s;
            }
            ConversationItem::ToolResult(t)
        }
        ConversationItem::Assistant(mut a) => {
            if let Some(s) = truncate_text_to_bytes(&a.content, max_bytes) {
                a.content = s;
            }
            ConversationItem::Assistant(a)
        }
        ConversationItem::User(mut u) => {
            for part in &mut u.content {
                if let ContentPart::Text { text } = part
                    && let Some(s) = truncate_text_to_bytes(text, max_bytes)
                {
                    *text = s;
                }
            }
            ConversationItem::User(u)
        }
        other => other,
    }
}
/// Char-boundary-safe prefix of `s` (incl. truncation marker) within `max_bytes`; `None` if `s` already fits.
fn truncate_text_to_bytes(s: &str, max_bytes: usize) -> Option<std::sync::Arc<str>> {
    if s.len() <= max_bytes {
        return None;
    }
    const MARKER_RESERVE: usize = 64;
    let keep = max_bytes.saturating_sub(MARKER_RESERVE);
    let mut end = keep.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    Some(std::sync::Arc::<str>::from(format!(
        "{}\n[... truncated {dropped} bytes to fit the compaction window ...]",
        &s[..end]
    )))
}
/// Tags injected by the runtime that should be stripped from user queries.
const SYSTEM_TAGS: &[&str] = &[
    "user_info",
    "project_layout",
    "git_status",
    "fork-context",
    "system-reminder",
    "agent-memory",
    "system_reminder",
    "background_context",
    "echoagent_personal_knowledge",
    "command-name",
    "command-message",
    "command-args",
    "rules",
];
/// Strip all known system/metadata tag blocks from `text`.
///
/// For each tag in [`SYSTEM_TAGS`], removes every `<tag>…</tag>` occurrence
/// (including content). Unclosed tags are left untouched.
fn strip_system_tags(text: &str) -> String {
    let mut result = text.to_string();
    for tag in SYSTEM_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = result.find(&open) {
            if let Some(rel_end) = result[start..].find(&close) {
                let end_pos = start + rel_end + close.len();
                result.replace_range(start..end_pos, "");
            } else {
                break;
            }
        }
    }
    result.trim().to_string()
}
/// Extracts the user query from a message that may contain metadata tags.
///
/// Looks for content within `<user_query>...</user_query>` tags.
/// If not found, strips known metadata tags (see [`SYSTEM_TAGS`]) and
/// returns the remaining content.
pub fn extract_user_query(text: &str) -> String {
    let stripped = strip_system_tags(text);
    if let Some(start) = stripped.find("<user_query>") {
        let content_start = start + "<user_query>".len();
        if let Some(end) = stripped[content_start..].find("</user_query>") {
            return stripped[content_start..content_start + end]
                .trim()
                .to_string();
        }
    }
    stripped
}
/// Extract the last actual user query text (stripping metadata tags).
///
/// Walks backward through the conversation, finds the last `User` item,
/// and extracts the raw query via [`extract_user_query`].
pub fn extract_last_user_query(conversation: &[ConversationItem]) -> Option<String> {
    conversation
        .iter()
        .rev()
        .find(|item| matches!(item, ConversationItem::User(_)))
        .map(|item| real_user_query(item))
        .filter(|q| !q.is_empty())
}
/// The continuation prompt added to the conversation after auto-compaction.
///
/// Stored here (rather than only in `echo-agent-runtime`) so that query-extraction
/// helpers in this crate can recognise and exclude it from "real user prompt"
/// lists without creating a circular dependency or hard-coding the text in two
/// places.
pub const AUTO_CONTINUE_PROMPT: &str = r#"Continue the conversation from where it left off without asking the user any further questions. Resume directly - do not acknowledge the summary, do not recap what was happening, do not preface with "I'll continue" or similar.
Pick up the last task as if the break never happened."#;
/// `false` twin: no preset in this build injects a bootstrap note.
fn is_bootstrap_reminder_text(_text: &str) -> bool {
    false
}
/// Return `true` when the *extracted* query text represents a synthetic
/// session-internal turn rather than a real human-authored prompt.
///
/// The cases handled:
/// - Empty string — the User item contained only metadata tags with no
///   `<user_query>` payload (bootstrap prefix on session start).
/// - `"__auto_continue__"` — the request-id sentinel sometimes stored inside
///   a `<user_query>` wrapper for identification purposes.
/// - The full [`AUTO_CONTINUE_PROMPT`] text — the actual message pushed into
///   the conversation after auto-compaction so the agent keeps progressing.
///   `extract_user_query` returns this as-is (no tags to strip), so it must
///   be explicitly excluded to avoid counting it as a real user query.
/// - A synthetic bootstrap tool-availability note wrapped in
///   `<system_reminder>` tags (optional presets only).
pub fn is_synthetic_extracted_query(text: &str) -> bool {
    text.is_empty()
        || text == "__auto_continue__"
        || text == AUTO_CONTINUE_PROMPT
        || is_bootstrap_reminder_text(text)
}
/// Classify whether a `ConversationItem` is a **real** user turn for
/// compaction purposes.
///
/// A user item is NOT a real user turn if any of the following hold:
/// 1. It is not a `User` variant at all.
/// 2. `synthetic_reason` is `Some(…)` (e.g. `SystemReminder`).
/// 3. It has no meaningful content: no images AND its extracted query
///    text is synthetic (empty, `__auto_continue__`, or the full
///    [`AUTO_CONTINUE_PROMPT`]).
///
/// Image-only user prompts (multimodal input with no text) ARE real
/// user turns — they must anchor the compaction boundary even though
/// they have no extractable text query.
///
/// This is the single source of truth for "real user" classification
/// in the compaction pipeline.
pub fn is_real_user_turn(item: &ConversationItem) -> bool {
    match item {
        ConversationItem::User(u) => {
            if u.synthetic_reason.is_some() {
                return false;
            }
            let has_images = u
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }));
            if has_images {
                return true;
            }
            let extracted = real_user_query(item);
            !is_synthetic_extracted_query(&extracted)
        }
        _ => false,
    }
}
/// Prefer persisted original input. Legacy sessions still strip host evidence tags.
fn real_user_query(item: &ConversationItem) -> String {
    if let ConversationItem::User(user) = item {
        if let Some(query) = &user.retrieval_query {
            return query.trim().to_owned();
        }
    }
    extract_user_query(&item.text_content())
}

/// Preserve human-authored memory evidence without turning retrieved documents or
/// organization answers into unrestricted, durable personal facts. Explicit
/// memory_manage operations remain governed by the user's separate instruction.
fn restricted_knowledge(item: &ConversationItem) -> bool {
    match item {
        ConversationItem::User(user) => user.knowledge_context.is_some() || user.content.iter().any(|part| {
            matches!(part,ContentPart::Text {text} if text.contains("<echoagent_personal_knowledge>"))
        }),
        ConversationItem::Assistant(assistant) => assistant.tool_calls.iter().any(|call| {
            let name=call.name.strip_prefix("echoagent_organization_memory__").unwrap_or(&call.name);
            name.starts_with("knowledge_")||name.starts_with("local_knowledge_") || (matches!(call.name.as_str(),"use_tool"|"multi_tool_use.parallel") && call.arguments.contains("echoagent_organization_memory"))
        }) || (assistant.content.contains("[Called tools:") && assistant.content.contains("echoagent_organization_memory__")),
        _ => false,
    }
}

pub fn personal_memory_conversation(conversation: Vec<ConversationItem>) -> Vec<ConversationItem> {
    // Once retrieved knowledge enters a model's context, later answers and
    // compacted summaries may derive from it even without another tool call.
    // Human-authored preferences remain eligible; generated evidence does not.
    let retrieved_context = conversation.iter().any(restricted_knowledge);
    conversation
        .into_iter()
        .filter_map(|mut item| {
            if retrieved_context && !is_real_user_turn(&item) {
                return None;
            }
            if is_real_user_turn(&item) {
                let query = real_user_query(&item);
                if let ConversationItem::User(user) = &mut item {
                    user.content = vec![ContentPart::Text {
                        text: std::sync::Arc::<str>::from(query),
                    }];
                }
            }
            Some(item)
        })
        .collect()
}

/// Extract all *real* user queries from a conversation, in order.
///
/// "Real" means the item passes [`is_real_user_turn`] — it has no
/// `synthetic_reason` and its extracted query text is not synthetic.
///
/// This is used by the session-end hooks and any logic that needs to
/// count or enumerate actual human-authored prompts without being
/// polluted by synthetic bootstrap messages or compaction artifacts.
pub fn extract_real_user_queries(conversation: &[ConversationItem]) -> Vec<String> {
    conversation
        .iter()
        .filter(|item| is_real_user_turn(item))
        .map(|item| real_user_query(item))
        .collect()
}
/// Extract the last *real* user query text from a conversation.
///
/// Unlike [`extract_last_user_query`], this function skips synthetic turns
/// (system reminders, metadata-only bootstrap prefixes, auto-continue
/// prompts) so it always returns content the user actually typed.
///
/// Returns `None` when no real user query is found.
pub fn extract_last_real_user_query(conversation: &[ConversationItem]) -> Option<String> {
    conversation
        .iter()
        .rev()
        .find(|item| is_real_user_turn(item))
        .map(|item| real_user_query(item))
}
/// Extract messages since the last user message in the conversation.
///
/// Walks backward from the end, collecting `Assistant` and `ToolResult` items
/// until a `User` item is hit. Tool results have their content replaced with
/// a placeholder to save space.
///
/// Returns the items in chronological order (reversed from the backward walk).
///
/// **Note**: This uses the raw `User` boundary which includes synthetic items
/// (system reminders, auto-continue prompts). For compaction, prefer
/// [`extract_messages_since_last_real_user`] which skips synthetic boundaries.
pub fn extract_messages_since_last_user(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    let mut messages: Vec<_> = conversation
        .iter()
        .rev()
        .take_while(|item| !matches!(item, ConversationItem::User(_)))
        .filter_map(|item| match item {
            ConversationItem::Assistant(a) => Some(ConversationItem::Assistant(a.clone())),
            ConversationItem::ToolResult(t) => Some(ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: t.tool_call_id.clone(),
                content: std::sync::Arc::<str>::from("Tool call omitted..."),
                images: Vec::new(),
            })),
            _ => None,
        })
        .collect();
    messages.reverse();
    messages
}
/// Extract messages since the last **real** user turn in the conversation.
///
/// Like [`extract_messages_since_last_user`], but the boundary is the last
/// item that passes [`is_real_user_turn`] — synthetic injections (system
/// warnings, auto-continue prompts) do NOT reset the boundary.
///
/// This prevents compaction from splitting an assistant/tool-result pair
/// that spans across a synthetic user injection, which would create an
/// orphaned `ToolResult` in the compacted history.
///
/// Tool results have their content replaced with a placeholder to save space.
/// Synthetic `User` items within the tail are omitted from the output.
///
/// Returns the items in chronological order.  Falls back to whole-tail
/// extraction (excluding system) if no real user turn exists.
pub fn extract_messages_since_last_real_user(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    let boundary_idx = conversation.iter().rposition(is_real_user_turn);
    let start = match boundary_idx {
        Some(idx) => idx + 1,
        None => 0,
    };
    conversation[start..]
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Assistant(a) => Some(ConversationItem::Assistant(a.clone())),
            ConversationItem::ToolResult(t) => Some(ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: t.tool_call_id.clone(),
                content: std::sync::Arc::<str>::from("Tool call omitted..."),
                images: Vec::new(),
            })),
            _ => None,
        })
        .collect()
}
#[derive(Clone, Copy)]
enum AgentMessagePosition {
    Only,
    BeforeHuman,
    AfterHuman,
}
/// Latest raw agent-authored anchor and its structural order relative to human input.
#[derive(Clone)]
pub struct AgentMessageAnchor {
    item: ConversationItem,
    position: AgentMessagePosition,
}
fn extract_latest_agent_message(conversation: &[ConversationItem]) -> Option<AgentMessageAnchor> {
    let agent_message_index = conversation.iter().rposition(|item| {
        matches!(
            item,
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
        )
    })?;
    let position = match conversation.iter().rposition(is_real_user_turn) {
        Some(human_index) if agent_message_index < human_index => AgentMessagePosition::BeforeHuman,
        Some(_) => AgentMessagePosition::AfterHuman,
        None => AgentMessagePosition::Only,
    };
    Some(AgentMessageAnchor {
        item: conversation[agent_message_index].clone(),
        position,
    })
}
fn extract_messages_since_last_compaction_anchor(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    let boundary = conversation.iter().rposition(|item| {
        is_real_user_turn(item)
            || matches!(
                item,
                ConversationItem::User(user)
                    if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
            )
    });
    let start = boundary.map_or(0, |idx| {
        if matches!(
            &conversation[idx],
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
        ) {
            idx
        } else {
            idx + 1
        }
    });
    conversation[start..]
        .iter()
        .filter_map(|item| match item {
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage) =>
            {
                Some(ConversationItem::User(user.clone()))
            }
            ConversationItem::Assistant(assistant) => {
                Some(ConversationItem::Assistant(assistant.clone()))
            }
            ConversationItem::ToolResult(result) => {
                Some(ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: result.tool_call_id.clone(),
                    content: std::sync::Arc::<str>::from("Tool call omitted..."),
                    images: Vec::new(),
                }))
            }
            _ => None,
        })
        .collect()
}
/// Summary of a running subagent for compaction context.
///
/// This is the compaction-layer type. The protocol-layer equivalent is
/// `ActiveSubagentSummary` in echo-agent-tools. The mapping between them
/// happens in `run_compact_inner()` (echo-agent-runtime).
#[derive(Clone)]
pub struct RunningSubagentSummary {
    /// The subagent's unique ID.
    pub subagent_id: String,
    /// The agent type name (e.g. "Explore", "general-purpose").
    pub subagent_type: String,
    /// Human-readable description of what the subagent is doing.
    pub description: String,
    /// Wall-clock elapsed time since the subagent was spawned, in milliseconds.
    pub elapsed_ms: u64,
}
/// Summary of a running background task for compaction context.
#[derive(Clone)]
pub struct BackgroundTaskSummary {
    pub task_id: String,
    pub command: String,
    pub status: String,
    /// Model-facing name of the tool that created this task (e.g. `monitor`).
    /// `None` omits it from the reminder.
    pub tool_name: Option<String>,
}
/// Summary of a connected MCP server for compaction context.
#[derive(Clone)]
pub struct CompactionServerSummary {
    pub name: String,
    pub tool_count: usize,
    pub description: Option<String>,
}
/// A dependency-free mirror of `TodoStatus` (echo-agent-tools), kept here so
/// this crate avoids that heavy dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoSummaryStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
impl TodoSummaryStatus {
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
    /// Mirrors `TodoStatus::tag()` in echo-agent-tools.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Pending => "[pending]",
            Self::InProgress => "[in_progress]",
            Self::Completed => "[completed]",
            Self::Cancelled => "[cancelled]",
        }
    }
}
/// Compaction-layer summary of a todo item. Protocol-layer equivalent is
/// `TodoItem` in echo-agent-tools.
#[derive(Clone)]
pub struct TodoSummary {
    pub id: String,
    pub content: String,
    pub status: TodoSummaryStatus,
}
/// Context captured at compaction time.
///
/// This is a pure data struct — rendering into system-reminder format is
/// handled by the consumer (e.g. `echo-agent-runtime`), which has access to
/// memory backends and other shell-specific dependencies.
pub struct CompactionStateContext {
    /// Retrieved evidence provenance survives summarization and session recovery.
    pub knowledge_context: Option<String>,
    /// Monotonic cwd generation; zero preserves the legacy compaction shape.
    pub cwd_generation: u64,
    /// Project instructions resolved for the latest destination cwd.
    pub destination_project_instructions: Option<String>,
    /// Latest agent-authored input and its order relative to the latest human turn.
    pub agent_message_anchor: Option<AgentMessageAnchor>,
    /// Messages since the latest human or agent-message compaction anchor.
    /// Runtime synthetic injections do not reset the boundary.
    pub recent_messages: Vec<ConversationItem>,
    /// The last real user query text (skips synthetic injections and
    /// auto-continue prompts).
    pub last_user_query: Option<String>,
    /// Files the agent edited this session (from agent_edited_paths).
    pub agent_edited_paths: Vec<String>,
    /// Running background tasks.
    pub running_tasks: Vec<BackgroundTaskSummary>,
    /// Subagents that are still running at compaction time.
    pub running_subagents: Vec<RunningSubagentSummary>,
    /// Connected MCP servers, for post-compaction system-reminder injection.
    pub connected_mcp_servers: Vec<CompactionServerSummary>,
    /// Todo list captured at compaction time, for post-compaction
    /// system-reminder injection.
    pub todos: Vec<TodoSummary>,
}
/// Live session state captured at compaction time, fed to
/// [`CompactionStateContext::build`].
#[derive(Default)]
pub struct CompactionInputs {
    pub cwd_generation: u64,
    pub destination_project_instructions: Option<String>,
    pub running_tasks: Vec<BackgroundTaskSummary>,
    pub running_subagents: Vec<RunningSubagentSummary>,
    pub agent_edited_paths: BTreeSet<String>,
    pub connected_mcp_servers: Vec<CompactionServerSummary>,
    pub todos: Vec<TodoSummary>,
}
impl CompactionStateContext {
    /// Build the state context from current session state.
    ///
    /// Uses a typed compaction boundary for the retained tail while keeping
    /// the last-query field human-only.
    pub async fn build(conversation: &[ConversationItem], inputs: CompactionInputs) -> Self {
        Self {
            knowledge_context: conversation
                .iter()
                .any(restricted_knowledge)
                .then(|| "retrieved-knowledge".to_owned()),
            cwd_generation: inputs.cwd_generation,
            destination_project_instructions: inputs.destination_project_instructions,
            agent_message_anchor: extract_latest_agent_message(conversation),
            recent_messages: extract_messages_since_last_compaction_anchor(conversation),
            last_user_query: extract_last_real_user_query(conversation),
            agent_edited_paths: inputs.agent_edited_paths.into_iter().collect(),
            running_tasks: inputs.running_tasks,
            running_subagents: inputs.running_subagents,
            connected_mcp_servers: inputs.connected_mcp_servers,
            todos: inputs.todos,
        }
    }
    /// Create a task summary from individual fields.
    pub fn task_summary(
        task_id: String,
        command: String,
        status: &str,
        tool_name: Option<String>,
    ) -> BackgroundTaskSummary {
        BackgroundTaskSummary {
            task_id,
            command,
            status: status.to_string(),
            tool_name,
        }
    }
    /// Return the **compaction view** of this context: a copy with the
    /// assistant/tool working tail dropped and the latest agent-message anchor
    /// kept.
    ///
    /// For a sub-agent with a single real user turn, `recent_messages` is the
    /// entire working transcript, and keeping it frees almost nothing while
    /// re-cueing the model to re-read the same files.
    pub fn for_compaction(&self) -> Self {
        Self {
            knowledge_context: self.knowledge_context.clone(),
            cwd_generation: self.cwd_generation,
            destination_project_instructions: self.destination_project_instructions.clone(),
            agent_message_anchor: self
                .agent_message_anchor
                .clone()
                .or_else(|| extract_latest_agent_message(&self.recent_messages)),
            recent_messages: Vec::new(),
            last_user_query: self.last_user_query.clone(),
            agent_edited_paths: self.agent_edited_paths.clone(),
            running_tasks: self.running_tasks.clone(),
            running_subagents: self.running_subagents.clone(),
            connected_mcp_servers: self.connected_mcp_servers.clone(),
            todos: self.todos.clone(),
        }
    }
}
/// Clean the compaction model's raw output into the plain-text `Summary:`
/// block that seeds the next turn.
///
/// Drafting scratchpad (a top-level `<analysis>` block, or a nested
/// `<analysis>`/`<summary>` wrapper / untagged markdown "**Analysis**" header
/// inside the summary) is stripped; control tokens echoed *within* the body
/// (the model sometimes quotes its own instruction under section 6) are
/// neutralized so they can't prime the next turn to re-emit a `<summary>`
/// block. A summary that already leads with a numbered section is preserved
/// verbatim even when it quotes `</analysis>`/`<summary>` in a later section.
pub fn format_compact_summary(summary: &str) -> String {
    let mut result = summary.to_string();
    while let Some(start) = result.find("<analysis>") {
        let is_leading = match result.find("<summary>") {
            Some(sp) => start < sp || result[sp + "<summary>".len()..start].trim().is_empty(),
            None => result[..start].trim().is_empty(),
        };
        if !is_leading {
            break;
        }
        match result[start..].find("</analysis>") {
            Some(rel) => {
                let end = start + rel + "</analysis>".len();
                result = format!("{}{}", &result[..start], &result[end..]);
            }
            None => {
                let drop_to = result[start..]
                    .find("<summary>")
                    .map_or(result.len(), |rel| start + rel);
                result = format!("{}{}", &result[..start], &result[drop_to..]);
                break;
            }
        }
    }
    if let Some(start) = result.find("<summary>")
        && let Some(end) = result.rfind("</summary>")
        && end > start
    {
        let before = result[..start].to_string();
        let after = result[end + "</summary>".len()..].to_string();
        let inner = strip_leading_scratchpad(result[start + "<summary>".len()..end].trim());
        result = format!("{before}Summary:\n{inner}{after}");
    }
    result = neutralize_compaction_control_tokens(&result);
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}
/// Peel leading drafting scratchpad off an extracted `<summary>` block.
///
/// A markdown "**Analysis**"-style header has no opening `<analysis>` tag for
/// step 1 to catch; it ends at an orphan `</analysis>`. Everything up to and
/// including the *last* `</analysis>` is dropped, so a scratchpad that itself
/// quotes `</analysis>` mid-reasoning is still removed whole. The peel is
/// skipped when the block already starts with a numbered section — including a
/// markdown-decorated one like `## 1.` or `**1.**` — so a `</analysis>` merely
/// echoed inside a real section never truncates the summary. Any leftover
/// leading `<summary>` wrapper is then unwrapped.
fn strip_leading_scratchpad(inner: &str) -> String {
    let mut s = inner.trim();
    let lead = s.trim_start_matches(['#', '*', '-', '>', ' ', '\t']);
    if !lead.starts_with(|c: char| c.is_ascii_digit())
        && let Some(pos) = s.rfind("</analysis>")
    {
        s = s[pos + "</analysis>".len()..].trim_start();
    }
    if let Some(rest) = s.strip_prefix("<summary>") {
        s = rest.trim_start();
    }
    s.to_string()
}
/// Defuse compaction-control tokens echoed inside a summary body by inserting
/// a zero-width space after `<`, so they can't be read as live tags by the next
/// turn. Mirrors `sanitize_evidence` in `goal_classifier.rs`. Closers first so
/// the inserted sentinel never re-matches.
fn neutralize_compaction_control_tokens(text: &str) -> String {
    text.replace("</summary>", "<\u{200b}/summary>")
        .replace("<summary>", "<\u{200b}summary>")
        .replace("</analysis>", "<\u{200b}/analysis>")
        .replace("<analysis>", "<\u{200b}analysis>")
        .replace("</summary_request>", "<\u{200b}/summary_request>")
        .replace("<summary_request>", "<\u{200b}summary_request>")
}
/// Clean tags via [`format_compact_summary`] and prepend the continuation
/// preamble. This is the user message content that replaces the compacted
/// conversation.
pub fn format_compact_summary_content(raw_summary: &str) -> String {
    let cleaned = format_compact_summary(raw_summary);
    format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation.\n\n{cleaned}"
    )
}
/// Floor for the cleaned seed (degenerate band observed at 75–264
/// chars; smallest healthy prod summary observed at 3,242 chars).
const MIN_SUMMARY_SEED_CHARS: usize = 500;
/// True when the cleaned summary seed is too small to plausibly carry the
/// task state of the conversation it would replace. Callers should
/// retry like a transient failure.
pub fn is_degenerate_summary(raw_summary: &str) -> bool {
    format_compact_summary(raw_summary).chars().count() < MIN_SUMMARY_SEED_CHARS
}
/// Cap (in `char`s) for the rejected-summary text captured on
/// [`CompactionAttempt::summary`].
pub const MAX_CAPTURED_SUMMARY_CHARS: usize = 8_192;
/// Bound captured text for the request artifact: whole when within `max_chars`,
/// else head + tail around an elision marker. Splits on `char` boundaries.
pub fn bound_captured_output(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars - head;
    let head_str: String = s.chars().take(head).collect();
    let tail_str: String = s.chars().skip(total - tail).collect();
    let elided = total - head - tail;
    format!("{head_str}\n\n…[{elided} chars elided]…\n\n{tail_str}")
}
/// Diagnostics for a single compaction model call (one retry-loop iteration),
/// persisted in order on the request artifact's `attempt_details` so a degraded
/// retry (a thinking-trace or hallucinated tools instead of a real summary)
/// isn't bumped invisibly.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionAttempt {
    /// 1-based attempt index, cumulative across input-ladder stages.
    pub attempt: u32,
    /// `"success"`, `"degenerate"`, `"deterministic"`, or `"transient"`.
    pub outcome: String,
    /// Raw char count of the content produced this attempt; `0` if none.
    pub summary_chars: u64,
    /// Raw rejected summary text on a degenerate attempt (bounded by
    /// [`bound_captured_output`]). `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Error detail on a failed (`deterministic` / `transient`) attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// Render a `<transcript_location>` pointer block.
///
/// The summary carrier embeds this XML pointer to the full raw transcript so
/// the model can re-read exact pre-compaction detail on demand instead of
/// carrying the working transcript in context. Carriers that include this
/// block splice it in right after `</summary_content>`. Carries its own
/// leading blank line.
pub fn format_transcript_location(path: &str) -> String {
    format!(
        "\n\n<transcript_location>\n\
         The full, unsummarized transcript of this conversation is saved at:\n{path}\n\
         If you need details that were dropped from the summary above (exact code, \
         error text, file contents, or earlier tool output), read this file to \
         recover them.\n\
         </transcript_location>"
    )
}
/// Wrap text in `<user_query>...</user_query>` tags.
///
/// This is the canonical wrapping used for user messages that contain
/// a query or compaction summary. Centralised here so both
/// `echo-agent-chat-state` and `echo-agent-runtime` share the same format.
pub fn wrap_user_query(text: impl Into<String>) -> String {
    let text = text.into();
    format!("<user_query>\n{text}\n</user_query>")
}
/// Input data for building a compacted conversation history.
///
/// All fields are plain data — no I/O, no network, no shell dependencies.
/// The caller is responsible for:
/// - Generating the `compaction_summary` via the LLM.
/// - Rendering the optional `system_reminder` (which may depend on
///   shell-specific backends such as memory search).
/// - Providing the `user_message_prefix` (e.g. `<user_info>` block).
pub struct CompactedHistoryInput<'a> {
    /// The original system message from the conversation.
    pub system_message: ConversationItem,
    /// The user-info / project-layout prefix (not wrapped in `<user_query>`).
    pub user_message_prefix: String,
    /// Pre-rendered AGENTS.md `<system-reminder>` block to re-inject after the
    /// user prefix. `None` means no project instructions to re-inject.
    /// This preserves project instructions verbatim across compaction.
    pub agents_md_reminder: Option<String>,
    /// State context snapshot taken before compaction cleared the conversation.
    pub state_context: &'a CompactionStateContext,
    /// The LLM-generated compaction summary text.
    pub compaction_summary: String,
    /// An optional pre-rendered `<system-reminder>` block to append after the
    /// summary. `None` means no state reminder is appended.
    pub system_reminder: Option<String>,
    /// When `true`, emit the compaction summary *before* recent messages.
    /// When `false` (the default), recent messages come first (echo-agent-build
    /// ordering).
    pub summary_before_recent: bool,
    /// Pre-built transcript hint appended to the summary (caller builds it via
    /// [`crate::CompactionMode::transcript_hint`] or
    /// [`format_transcript_location`]). `None` to omit. Appended to BOTH the
    /// carrier and the echo-agent-build summary.
    pub transcript_hint: Option<String>,
    /// Number of summaries generated so far for this user query, *including*
    /// the one being built. Rendered verbatim into the carrier's
    /// "Total summaries generated so far …" footer. Ignored by the echo-agent-build
    /// (`summary_before_recent == false`) path. Callers that don't track a
    /// counter pass `1`.
    pub summary_count: u64,
}
/// `None` twin: the alternate carrier format is not compiled in.
fn summary_before_recent_carrier(_input: &CompactedHistoryInput<'_>) -> Option<String> {
    None
}
/// This is a pure function with no I/O. It mirrors exactly what
/// `run_compact_inner` in `echo-agent-runtime` assembles inline, but is
/// independently testable.
pub fn build_compacted_history(input: CompactedHistoryInput<'_>) -> Vec<ConversationItem> {
    let carrier = summary_before_recent_carrier(&input);
    let summary_first = carrier.is_some();
    let mut summary_item = carrier.map(ConversationItem::user_meta).unwrap_or_else(|| {
        let mut formatted_summary = format_compact_summary_content(&input.compaction_summary);
        if let Some(ref hint) = input.transcript_hint {
            formatted_summary.push_str(hint);
        }
        ConversationItem::user_meta(formatted_summary)
    });
    if let ConversationItem::User(user) = &mut summary_item {
        user.knowledge_context = input.state_context.knowledge_context.clone();
    }
    let mut compacted: Vec<ConversationItem> = vec![
        input.system_message,
        ConversationItem::user_meta(input.user_message_prefix),
    ];
    let project_instructions = if input.state_context.cwd_generation == 0 {
        input.agents_md_reminder.as_ref()
    } else {
        input
            .state_context
            .destination_project_instructions
            .as_ref()
    };
    if let Some(reminder) = project_instructions {
        compacted.push(ConversationItem::project_instructions(reminder.clone()));
    }
    let anchor = input
        .state_context
        .agent_message_anchor
        .clone()
        .filter(|_| {
            !input.state_context.recent_messages.iter().any(|item| {
                matches!(
                    item,
                    ConversationItem::User(user)
                        if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
                )
            })
        });
    if let Some(anchor) = anchor
        .as_ref()
        .filter(|anchor| matches!(anchor.position, AgentMessagePosition::BeforeHuman))
    {
        compacted.push(anchor.item.clone());
    }
    if let Some(ref last_query) = input.state_context.last_user_query {
        compacted.push(ConversationItem::user(wrap_user_query(last_query)));
    }
    if let Some(anchor) =
        anchor.filter(|anchor| !matches!(anchor.position, AgentMessagePosition::BeforeHuman))
    {
        compacted.push(anchor.item);
    }
    if summary_first {
        compacted.push(summary_item);
        for msg in input.state_context.recent_messages.iter().cloned() {
            compacted.push(msg);
        }
    } else {
        for msg in input.state_context.recent_messages.iter().cloned() {
            compacted.push(msg);
        }
        compacted.push(summary_item);
    }
    if let Some(ref reminder) = input.system_reminder {
        compacted.push(ConversationItem::system_reminder(reminder.clone()));
    }
    compacted
}
/// Result of sanitizing a compacted conversation history.
pub struct SanitizeResult {
    /// The sanitized conversation items.
    pub items: Vec<ConversationItem>,
    /// `tool_call_id`s that were stripped because no preceding assistant
    /// `tool_calls` entry matched them.
    pub stripped_tool_call_ids: Vec<String>,
}
/// Check whether a compacted conversation satisfies the provider invariant:
///
/// > Every `ToolResult` must have a matching **preceding**
/// > `Assistant.tool_calls[].id`.
///
/// Returns the `tool_call_id`s of any `ToolResult` items that violate
/// the invariant (empty when the history is valid).
///
/// This is a read-only check — it does not modify the conversation.
pub fn validate_compacted_history(items: &[ConversationItem]) -> Vec<String> {
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut invalid_ids = Vec::new();
    for item in items {
        match item {
            ConversationItem::Assistant(a) => {
                for tc in &a.tool_calls {
                    seen_ids.insert(&tc.id);
                }
            }
            ConversationItem::ToolResult(tr) if !seen_ids.contains(tr.tool_call_id.as_str()) => {
                invalid_ids.push(tr.tool_call_id.clone());
            }
            _ => {}
        }
    }
    invalid_ids
}
/// Sanitize a compacted conversation by removing orphaned `ToolResult` items.
///
/// Enforces the provider-critical invariant via a left-to-right scan:
///
/// > Every `ToolResult` in the history must have a matching **preceding**
/// > `Assistant.tool_calls[].id`.
///
/// As each `Assistant` is encountered, its tool-call IDs are added to a
/// seen set.  Any `ToolResult` whose `tool_call_id` is not yet in the
/// seen set is stripped (this catches both "no matching assistant" and
/// "result appears before its call").
///
/// **Explicit non-goal**: `Assistant` messages with `tool_calls` but no
/// matching `ToolResult` are NOT stripped — that can be a legitimate
/// in-flight or partially-repaired state and is not the invariant that
/// causes provider 400 errors.
pub fn sanitize_compacted_history(items: Vec<ConversationItem>) -> SanitizeResult {
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stripped_tool_call_ids = Vec::new();
    let sanitized = items
        .into_iter()
        .filter(|item| match item {
            ConversationItem::Assistant(a) => {
                for tc in &a.tool_calls {
                    seen_ids.insert(tc.id.as_ref().to_owned());
                }
                true
            }
            ConversationItem::ToolResult(tr) => {
                if seen_ids.contains(&tr.tool_call_id) {
                    true
                } else {
                    stripped_tool_call_ids.push(tr.tool_call_id.clone());
                    false
                }
            }
            _ => true,
        })
        .collect();
    SanitizeResult {
        items: sanitized,
        stripped_tool_call_ids,
    }
}
/// What [`repair_history`] changed; all-zero/empty means nothing was rewritten.
#[derive(Debug, Clone, Default)]
pub struct HistoryRepairReport {
    /// Duplicate `ToolResult` entries removed.
    pub duplicates_removed: usize,
    /// `tool_call_id`s of orphaned/displaced `ToolResult`s stripped — the
    /// shape behind "unexpected `tool_use_id` found in `tool_result` blocks".
    pub stripped_tool_result_ids: Vec<String>,
    /// Synthetic `ToolResult`s inserted for unanswered `tool_calls`.
    pub synthetic_results_inserted: usize,
}
impl HistoryRepairReport {
    /// Whether the repair modified the conversation.
    pub fn changed(&self) -> bool {
        self.duplicates_removed > 0
            || !self.stripped_tool_result_ids.is_empty()
            || self.synthetic_results_inserted > 0
    }
}
/// Repair provider tool-pairing violations in a conversation (e.g. orphaned
/// `ToolResult`s left by a torn JSONL line, which 400 on every request).
/// Three passes: [`dedup_duplicate_tool_results`],
/// [`strip_displaced_tool_results`], then [`repair_dangling_tool_calls`] to
/// backfill synthetic results for calls the stripping left unanswered.
/// Pure and idempotent.
pub fn repair_history(items: &mut Vec<ConversationItem>) -> HistoryRepairReport {
    let duplicates_removed = echo_agent_sampling_types::dedup_duplicate_tool_results(items);
    let stripped_tool_result_ids = strip_displaced_tool_results(items);
    let synthetic_results_inserted = echo_agent_sampling_types::repair_dangling_tool_calls(
        items,
        echo_agent_sampling_types::DanglingToolCallReason::HarnessHalted {
            class: "history_repair",
        },
    );
    HistoryRepairReport {
        duplicates_removed,
        stripped_tool_result_ids,
        synthetic_results_inserted,
    }
}
/// Strip `ToolResult`s that are not in the contiguous run immediately
/// following the `Assistant` declaring their `tool_call_id` — both orphans
/// (owner gone: the bricked-session case) and displaced results. Returns the
/// stripped ids in order.
///
/// Deliberately stricter than [`sanitize_compacted_history`]'s "matching id
/// anywhere before" (providers require adjacency), and deliberately the same
/// contiguous-run rule as [`repair_dangling_tool_calls`] /
/// [`dedup_duplicate_tool_results`] so the [`repair_history`] passes agree on
/// which calls are answered (a leniency mismatch would make the dangling pass
/// insert synthetic duplicates next to kept results).
pub fn strip_displaced_tool_results(items: &mut Vec<ConversationItem>) -> Vec<String> {
    let mut run_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stripped = Vec::new();
    items.retain(|item| match item {
        ConversationItem::Assistant(a) => {
            run_ids = a
                .tool_calls
                .iter()
                .map(|tc| tc.id.as_ref().to_owned())
                .collect();
            true
        }
        ConversationItem::ToolResult(tr) => {
            if run_ids.contains(&tr.tool_call_id) {
                true
            } else {
                stripped.push(tr.tool_call_id.clone());
                false
            }
        }
        _ => {
            run_ids.clear();
            true
        }
    });
    stripped
}
#[cfg(test)]
#[path = "compaction_utils_tests.rs"]
mod tests;
