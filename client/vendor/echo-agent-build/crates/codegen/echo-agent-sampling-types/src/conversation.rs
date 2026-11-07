//! API-agnostic conversation representation.
//!
//! The types here capture a superset of what the backends accept, so a caller
//! can switch between them by configuration. Each backend owns its own wire
//! conversion in a sibling module.

mod chat_completions;
mod messages;
mod responses;

pub use chat_completions::{conversation_item_to_chat_message, conversation_to_chat_messages};
pub use messages::build_messages_request;
pub use responses::{
    extra_tool_entries, patch_reasoning_text_types, response_to_conversation_items,
};

use std::sync::Arc;

const STRUCTURED_OUTPUT_SCHEMA_NAME: &str = "structured_output";

/// Truncate to at most `max_bytes`, walking back to a char boundary. Plain
/// `&s[..n]` panics when `n` lands inside a multi-byte character, which
/// tool-call arguments routinely contain. `pub` for `echo-agent-runtime`.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A provider that validates `function.arguments` rejects the whole request,
/// so one malformed call from an earlier turn breaks every turn after it. The
/// matching `tool_result` keeps the original text, so the model can recover.
fn sanitize_tool_arguments(id: &str, name: &str, arguments: Arc<str>) -> Arc<str> {
    // `IgnoredAny` avoids building a DOM on a path that runs for every call.
    if serde_json::from_str::<serde::de::IgnoredAny>(&arguments).is_err() {
        tracing::warn!(
            tool_call_id = id,
            tool_name = name,
            args_preview = truncate_bytes(&arguments, 200),
            "Tool call has invalid JSON arguments; replacing with {{}} to prevent provider 400"
        );
        Arc::<str>::from("{}")
    } else {
        arguments
    }
}

use serde::{Deserialize, Serialize};

use crate::rs;
use crate::tool_overrides::{ToolOverrides, WebSearchOptions, XSearchOptions, drop_empty};
use crate::types::{
    ChatCompletionRequest, ChatContentBlock, ChatRequestMessage, ChatResponseMessage, FinishReason,
    ImageUrl, MessageContent, Role, ToolCallRequest, ToolChoice, ToolDefinition, TraceContext,
    Usage,
};

// ============================================================================
// Core Conversation Types
// ============================================================================

/// A single item in a conversation - the unified internal representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    /// System instructions/prompt
    System(SystemItem),
    /// User input message
    User(UserItem),
    /// Assistant response
    Assistant(AssistantItem),
    /// Tool/function result
    ToolResult(ToolResultItem),
    /// A tool call executed server-side by the backend agentic sampler
    /// (e.g. web search, X search, code interpreter). These are NOT
    /// executed by the client — the server already ran them and fed
    /// results into the model's context. Stored so they can be:
    /// 1. Persisted to chat_history.jsonl for session replay/fork
    /// 2. Sent back to the Responses API as input items for context continuity
    /// 3. Rendered by the pager (search queries, sources, etc.)
    BackendToolCall(BackendToolCallItem),
    /// A reasoning item from the Responses API, stored as a sibling of the
    /// assistant message so that:
    ///
    /// 1. N parallel `tco_*` reasoning items (one per backend tool call)
    ///    round-trip losslessly without last-write-wins clobbering.
    /// 2. The interleaved order of `[reasoning, tool_call, reasoning, ...,
    ///    message]` produced by the model is preserved byte-stable across
    ///    turns, which is what lets the server-side prefix KV-cache hit.
    ///
    /// Wraps `rs::ReasoningItem` directly (symmetric with `BackendToolCall`
    /// wrapping `rs::WebSearchToolCall` etc.) so no field is dropped on the
    /// way through.
    Reasoning(rs::ReasoningItem),
}

/// System message content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemItem {
    pub content: Arc<str>,
}

/// Reason why a `UserItem` was synthesized by the runtime rather than typed
/// by a real user.  Stored alongside the item so downstream code (pruning,
/// replay, analytics) can distinguish synthetic injections from real input
/// without parsing message text.
///
/// Serialized as a lowercase string (e.g. `"auto_continue"`).
/// Unknown variants (from future clients or removed historical tags such as
/// `"doom_loop_warning"`) deserialize as [`SyntheticReason::Unknown`]
/// so old clients can still read sessions written by newer versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticReason {
    /// Metadata injected by the compaction pipeline (e.g., re-read file
    /// contents). Not real user input.
    CompactionMeta,
    /// Runtime-injected `<system-reminder>` message. Not real user input.
    SystemReminder,
    /// Project-level instruction message (AGENTS.md / CLAUDE.md) injected at
    /// session spawn. Invariant: once placed, never replaced (would bust the
    /// KV-cache prefix).
    ProjectInstructions,
    /// Injected by the auto-continue logic after compaction so the agent
    /// keeps working.  Not real user input.
    AutoContinue,
    /// Injected by the auto-recovery logic after a transient tool failure
    /// to retry the operation.  Not real user input.
    AutoRecovery,
    /// User-initiated mid-turn interjection sent via Ctrl+Enter while the
    /// model was actively running.  Injected between tool batches so the
    /// model sees it as steering context without canceling the turn.
    Interjection,
    /// Model-authored input sent by another agent.
    #[serde(alias = "parent_agent_message")]
    AgentMessage,
    /// Auto-wake synthetic prompt injected when a background bash task
    /// completed.  Wakes the agent for a new turn.
    TaskCompleted,
    /// Auto-wake synthetic prompt injected when a background subagent
    /// completed.  Wakes the agent for a new turn.
    SubagentCompleted,
    /// Idle-gated notification drain: batched monitor events and/or bash
    /// task completions drained when the session is idle.  Wakes the agent.
    NotificationDrain,
    /// Goal orchestrator summary turn.  The goal system triggers a model
    /// turn so it can print visible progress.  Wakes the agent.
    GoalSummary,
    /// Goal-achievement classifier nudge injected after the classifier
    /// rejects an `update_goal(completed: true)` attempt. Wakes the
    /// agent with a "not yet achieved — keep working" reminder pointing
    /// at the persisted details file.
    GoalClassifierNudge,
    /// Scheduled task (`/loop`) prompt fired by the scheduler.  Wakes the
    /// agent.
    SchedulerFired,
    /// Feedback from a `Stop`/`SubagentStop` hook that blocked the agent from
    /// stopping. Injected in-turn so the model keeps working within the same turn.
    StopHookFeedback,
    /// Working-directory switch context appended after a session relocation.
    /// Carries a generation marker so recovery can detect an existing append.
    WorkingDirectorySwitch,
    /// Catch-all for unknown/future variants.  Preserves forward compatibility
    /// so older clients can deserialize sessions written by newer versions.
    #[serde(other)]
    Unknown,
}

impl SyntheticReason {
    /// Whether a user item with this reason **starts a prompt turn** — i.e.
    /// the turn pipeline pushed it while consuming a `prompt_index` slot
    /// (auto-wake and other server-initiated turns), as opposed to a
    /// mid-turn injection that never incremented the index.
    ///
    /// Used by [`conversation_truncate_for_prompt`]'s counting fallback for
    /// items persisted before [`UserItem::prompt_index`] existed. Exhaustive
    /// match — adding a variant forces an explicit decision here.
    ///
    /// `GoalSummary` is deliberately `false`: the same reason tags both the
    /// legacy goal-continuation *turn* (index-consuming) and the in-turn goal
    /// directive (mid-turn). Unknown future reasons fail safe as boundaries so
    /// older readers cannot merge a newer conversational origin into a prior turn.
    pub fn starts_prompt_turn(&self) -> bool {
        match self {
            Self::AgentMessage
            | Self::Unknown
            | Self::TaskCompleted
            | Self::SubagentCompleted
            | Self::NotificationDrain
            | Self::GoalClassifierNudge
            | Self::SchedulerFired => true,
            Self::CompactionMeta
            | Self::SystemReminder
            | Self::ProjectInstructions
            | Self::AutoContinue
            | Self::AutoRecovery
            | Self::Interjection
            | Self::GoalSummary
            | Self::StopHookFeedback
            | Self::WorkingDirectorySwitch => false,
        }
    }
}

/// How the user *fatally* interrupted (cancelled) the turn immediately
/// preceding this *real* user message. Set only on genuine user messages
/// (`synthetic_reason == None`) that directly follow a cancelled turn, so
/// downstream code (replay, analytics, the model itself) can see that the user
/// redirected after stopping work — without parsing message text.
///
/// Reserved for the *fatal* user-interrupt causes that end the turn:
/// `mid_turn_abort` (ESC / Ctrl+C), `permission_rejected`, `permission_cancelled`.
/// A mid-turn *interjection* is deliberately NOT represented here — it does not
/// cancel the turn and is captured on its own message via
/// [`SyntheticReason::Interjection`] (and the `interjected` telemetry event).
/// Automatic terminations (hook-denied, max-turns) are not user interrupts
/// and never set this.
///
/// Serialized as a lowercase string. Unknown variants from future writers
/// deserialize as [`PriorTurnInterrupt::Unknown`] for forward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorTurnInterrupt {
    /// Previous turn was aborted mid-flight (ESC / Ctrl+C while streaming or
    /// running tools).
    MidTurnAbort,
    /// User clicked "No" on a permission prompt, ending the previous turn.
    PermissionRejected,
    /// User cancelled a permission prompt (Cmd+C), ending the previous turn.
    PermissionCancelled,
    /// Forward-compat catch-all for variants written by newer clients.
    #[serde(other)]
    Unknown,
}

/// User message with text and optional images
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserItem {
    pub content: Vec<ContentPart>,
    /// Original human query, separate from retrieved evidence and generated context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_query: Option<String>,
    /// Retrieved knowledge turns cannot become unrestricted personal memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_context: Option<String>,
    /// Set when this item was synthesized by the runtime rather than typed by
    /// a real user.  `None` for all genuine user messages.
    ///
    /// Uses `skip_serializing_if` so old JSONL sessions that lack this field
    /// deserialize correctly (`serde(default)` fills in `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_reason: Option<SyntheticReason>,
    /// Relocation generation for a working-directory switch reminder.
    /// Structural metadata keeps recovery dedup independent of reminder text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_generation: Option<u64>,
    /// Set on a genuine user message that directly follows a user-interrupted
    /// turn (see [`PriorTurnInterrupt`]). `None` for synthetic messages and for
    /// real messages that did not follow an interrupt. `skip_serializing_if`
    /// keeps old sessions/round-trips byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_turn_interrupt: Option<PriorTurnInterrupt>,
    /// Prompt-turn index this user item started, recorded at push time. Same
    /// coordinate space as the session's `prompt_index` / rewind targets
    /// (every `handle_prompt` turn counts, synthetic-origin turns included)
    /// and the `promptIndex` meta on `UserMessageChunk` updates.
    ///
    /// `None` for items that do not start a turn (the `<user_info>` preamble,
    /// mid-turn synthetic injections, freshly rebuilt compaction messages —
    /// though tail messages cloned into a compacted history keep their
    /// markers) and for items persisted before this field existed. Rewind
    /// truncation prefers a present value over counting
    /// ([`conversation_truncate_for_prompt`]).
    ///
    /// Caveat: session resume recounts `prompt_index` from `updates.jsonl`,
    /// which can drift from this coordinate (interjection echoes, image-only
    /// prompts), so markers stamped before and after a restart may disagree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_index: Option<usize>,
}

/// Assistant response with tool calls.
///
/// Reasoning items, when present, sit beside this item as
/// `ConversationItem::Reasoning(_)` siblings preceding the assistant turn —
/// not bundled here. That keeps N parallel reasoning items (e.g. `tco_*`
/// blobs from parallel backend tool calls) lossless and preserves the
/// interleaved order the model emits. Old sessions on disk may still carry
/// a `reasoning` field; serde silently ignores it on read (no
/// `deny_unknown_fields`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantItem {
    /// Text content of the response
    pub content: Arc<str>,
    /// Tool calls made by the assistant (client must execute these locally)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The model that generated this response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "system_fingerprint",
        deserialize_with = "crate::serde_helpers::empty_string_as_none"
    )]
    pub model_fingerprint: Option<String>,
    /// The reasoning effort the server applied for this response, echoed on
    /// `response.reasoning.effort` (Responses API). Stored beside
    /// `model_id`/`model_fingerprint` so per-response effort survives
    /// mid-session model/effort switches. `None` for synthetic items and
    /// backends that don't echo it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::ReasoningEffort>,
}

/// Tool result message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultItem {
    /// ID of the tool call this is responding to
    pub tool_call_id: String,
    /// The result content
    pub content: Arc<str>,
    /// Inline images associated with this tool result (e.g. from `read_file`
    /// on an image/PDF). When non-empty, the API conversion layers embed
    /// these directly in the tool result message instead of requiring a
    /// separate follow-up user message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ContentPart>,
}

/// A server-side tool call from the backend agentic sampler.
///
/// Wraps the typed Responses API output items so they can be round-tripped
/// back to the server and rendered by the pager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendToolCallItem {
    /// The specific backend tool that was called.
    pub kind: BackendToolKind,
}

impl BackendToolCallItem {
    /// The backend-tool-call id (the `id` field on the underlying
    /// `rs::WebSearchToolCall` / `rs::CustomToolCall` /
    /// `rs::CodeInterpreterToolCall`). Used by the legacy-session
    /// upgrader to dedupe against the same call when it also appears
    /// inside a sibling assistant's `raw_output` array.
    pub fn id(&self) -> &str {
        match &self.kind {
            BackendToolKind::WebSearch(ws) => ws.id.as_str(),
            BackendToolKind::XSearch(ct) => ct.id.as_str(),
            BackendToolKind::CodeInterpreter(ci) => ci.id.as_str(),
        }
    }

    /// Human-readable summary for token estimation and text extraction.
    pub fn text_summary(&self) -> String {
        match &self.kind {
            BackendToolKind::WebSearch(ws) => {
                let action_desc = match &ws.action {
                    rs::WebSearchToolCallAction::Search(s) => format!("search: {}", s.query),
                    rs::WebSearchToolCallAction::OpenPage(o) => {
                        format!("open: {}", o.url.as_deref().unwrap_or("?"))
                    }
                    rs::WebSearchToolCallAction::Find(f)
                    | rs::WebSearchToolCallAction::FindInPage(f) => {
                        format!("find \"{}\" in {}", f.pattern, f.url)
                    }
                };
                format!("[backend web_search] {action_desc}")
            }
            BackendToolKind::XSearch(ct) => {
                format!("[backend x_search] {}({})", ct.name, ct.input)
            }
            BackendToolKind::CodeInterpreter(ci) => {
                let code_preview = ci
                    .code
                    .as_deref()
                    .map(|c| {
                        if c.len() > 100 {
                            format!("{}...", &c[..100])
                        } else {
                            c.to_string()
                        }
                    })
                    .unwrap_or_default();
                format!("[backend code_interpreter] {code_preview}")
            }
        }
    }
}

/// Discriminated union of backend-executed tool call types.
///
/// Each variant wraps the native Responses API struct, enabling
/// zero-copy round-tripping when building subsequent API requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool_type", rename_all = "snake_case")]
pub enum BackendToolKind {
    /// Server-side web search (query + sources).
    WebSearch(rs::WebSearchToolCall),
    /// Server-side X/Twitter search (keyword, semantic, user, thread).
    XSearch(rs::CustomToolCall),
    /// Server-side code interpreter execution.
    CodeInterpreter(rs::CodeInterpreterToolCall),
}

// ============================================================================
// Content Parts
// ============================================================================

/// A part of message content - text, image, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text content
    Text { text: Arc<str> },
    /// Image content (URL or base64 data URI)
    Image { url: Arc<str> },
}

// ============================================================================
// Reasoning Content
// ============================================================================

/// Reasoning/thinking content from the model.
/// Structured to support both plain text (chat completions) and
/// encrypted reasoning (responses API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningContent {
    /// Plain text reasoning (always available for display)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Arc<str>>,
    /// Encrypted reasoning content (Responses API only)
    /// This can be passed back to the API for context continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<Arc<str>>,
    /// Original reasoning item ID from the Responses API.
    /// Required when replaying reasoning items in subsequent turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Arc<str>>,
}

impl ReasoningContent {
    /// Create from plain text reasoning (chat completions style)
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(Arc::<str>::from(text.into())),
            encrypted: None,
            id: None,
        }
    }

    /// Create from encrypted content (responses API style)
    pub fn from_encrypted(encrypted: impl Into<String>) -> Self {
        Self {
            text: None,
            encrypted: Some(Arc::<str>::from(encrypted.into())),
            id: None,
        }
    }

    /// Build from a Responses API `ReasoningItem`.
    /// Prefers `content` (full raw reasoning) over `summary` for the text field;
    /// in practice the API populates one or the other, never both.
    pub fn from_reasoning_item(r: &rs::ReasoningItem) -> Option<Self> {
        let text = Self::join_content(&r.content).or_else(|| Self::join_summary(&r.summary));
        if text.is_none() && r.encrypted_content.is_none() {
            return None;
        }
        Some(Self {
            text,
            encrypted: r.encrypted_content.as_deref().map(Arc::<str>::from),
            id: Some(Arc::<str>::from(r.id.as_str())),
        })
    }

    /// Check if there's any reasoning content
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.encrypted.is_none()
    }

    fn join_content(content: &Option<Vec<rs::ReasoningTextContent>>) -> Option<Arc<str>> {
        let parts = content.as_ref()?;
        let joined: String = parts
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (!joined.is_empty()).then_some(Arc::<str>::from(joined))
    }

    fn join_summary(summary: &[rs::SummaryPart]) -> Option<Arc<str>> {
        let joined: String = summary
            .iter()
            .map(|p| {
                let rs::SummaryPart::SummaryText(st) = p;
                st.text.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!joined.is_empty()).then_some(Arc::<str>::from(joined))
    }
}

// ============================================================================
// Tool Definitions and Calls
// ============================================================================

/// A tool call made by the assistant that the client must execute locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool call
    pub id: Arc<str>,
    /// Name of the function to call
    pub name: String,
    /// JSON-encoded arguments
    pub arguments: Arc<str>,
}

/// Tool/function definition for the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Name of the tool
    pub name: String,
    /// Description of what the tool does
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the parameters
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostedTool {
    WebSearch { options: Option<WebSearchOptions> },
    XSearch { options: Option<XSearchOptions> },
}

impl HostedTool {
    pub fn wire_name(&self) -> &'static str {
        match self {
            HostedTool::WebSearch { .. } => "web_search",
            HostedTool::XSearch { .. } => "x_search",
        }
    }
}

/// Resolve `overrides` onto the hosted tools in place so the serialized request matches the returned
/// echo. Empty options normalize to absent (via `drop_empty`), so a stray `{}` never clears a seeded
/// bound. Returns the applied overrides.
pub fn apply_tool_overrides(
    tools: &mut [HostedTool],
    overrides: Option<&ToolOverrides>,
) -> ToolOverrides {
    let mut applied = ToolOverrides::default();
    for tool in tools.iter_mut() {
        match tool {
            HostedTool::XSearch { options } => {
                if let Some(x) = drop_empty(
                    overrides.and_then(|o| o.x_search.clone()),
                    XSearchOptions::is_empty,
                ) {
                    *options = Some(x);
                }
                applied.x_search = drop_empty(options.clone(), XSearchOptions::is_empty);
            }
            HostedTool::WebSearch { options } => {
                if let Some(w) = drop_empty(
                    overrides.and_then(|o| o.web_search.clone()),
                    WebSearchOptions::is_empty,
                ) {
                    *options = Some(w);
                }
                applied.web_search = drop_empty(options.clone(), WebSearchOptions::is_empty);
            }
        }
    }
    applied
}

impl From<ToolDefinition> for ToolSpec {
    fn from(td: ToolDefinition) -> Self {
        Self {
            name: td.function.name,
            description: td.function.description,
            parameters: td.function.parameters,
        }
    }
}

// ============================================================================
// Conversation Request
// ============================================================================

/// Live, caller-owned policy applied immediately before each transport attempt.
/// The sampler stays independent of the caller's configuration and storage.
pub trait ConversationRequestPreflight: std::fmt::Debug + Send + Sync {
    fn apply(&self, request: &mut ConversationRequest) -> Result<(), &'static str>;
}

/// A complete conversation request that can be sent to either API.
#[derive(Debug, Clone, Default)]
pub struct ConversationRequest {
    pub preflight: Option<Arc<dyn ConversationRequestPreflight>>,
    /// The conversation items (messages)
    pub items: Vec<ConversationItem>,
    /// Available tools (client-side, sent as Function definitions)
    pub tools: Vec<ToolSpec>,
    /// Backend-hosted tools (sent as native Responses API tool types).
    /// These are executed server-side by the agentic sampler during inference.
    pub hosted_tools: Vec<HostedTool>,
    /// Tool choice behavior
    pub tool_choice: Option<ConversationToolChoice>,
    /// Model to use (if not using client default)
    pub model: Option<String>,
    /// Sampling temperature
    pub temperature: Option<f32>,
    /// Maximum output tokens
    pub max_output_tokens: Option<u32>,
    /// Top-p sampling
    pub top_p: Option<f32>,
    /// Custom headers for EchoAgent tracking
    pub x_echo_agent_conv_id: Option<String>,
    pub x_echo_agent_req_id: Option<String>,
    pub x_echo_agent_session_id: Option<String>,
    pub x_echo_agent_turn_idx: Option<String>,
    pub x_echo_agent_agent_id: Option<String>,
    pub x_echo_agent_deployment_id: Option<String>,
    pub x_echo_agent_user_id: Option<String>,
    /// Optional opaque tracing context (e.g., where to persist the finalized request payload).
    /// Consumers downcast via `trace.as_ref().unwrap().as_any().downcast_ref::<T>()`.
    pub trace: Option<Box<dyn TraceContext>>,
    /// Reasoning effort level for reasoning models.
    pub reasoning_effort: Option<crate::ReasoningEffort>,
    /// JSON Schema for structured output (strict mode).
    pub json_schema: Option<serde_json::Value>,
    /// Sticky routing key for prompt-cache reuse; overrides `x_echo_agent_conv_id` for routing.
    pub prompt_cache_key: Option<String>,
}

impl ConversationRequest {
    /// Strip every image; returns the stripped URLs.
    pub fn strip_images(&mut self) -> Vec<Arc<str>> {
        strip_images_where(&mut self.items, |_| true)
    }
}

/// Strip only `urls`. Unlisted images (compaction, newer turns) stay.
/// Returns the number of stripped occurrences (one per replaced part, so a
/// URL stored twice counts twice).
///
/// Invariant: replaces parts in place, never adds or removes a
/// `ConversationItem` (the `&mut [_]` signature cannot resize); chat-state
/// relies on this to skip turn-capture rebasing.
pub fn strip_images_by_url(items: &mut [ConversationItem], urls: &[Arc<str>]) -> usize {
    strip_images_where(items, |url| urls.iter().any(|u| u.as_ref() == url)).len()
}

/// Replaces a stripped user image. Deliberately verbose, like the eviction
/// placeholder: a silently-stripped image otherwise induces confident
/// hallucination of its contents.
pub const IMAGE_STRIP_PLACEHOLDER: &str = "[image removed — the server could not process it; \
     its contents are unavailable. Ask the user to re-attach the image if it is still needed.]";

/// User images become [`IMAGE_STRIP_PLACEHOLDER`]; tool-result images are
/// dropped (a placeholder there is invisible to the conversion layers).
fn strip_images_where(
    items: &mut [ConversationItem],
    mut should_strip: impl FnMut(&str) -> bool,
) -> Vec<Arc<str>> {
    let mut stripped = Vec::new();
    for item in items {
        match item {
            ConversationItem::User(user) => {
                for part in &mut user.content {
                    match part {
                        ContentPart::Image { url } if should_strip(url) => {
                            stripped.push(Arc::clone(url));
                            *part = ContentPart::Text {
                                text: Arc::<str>::from(IMAGE_STRIP_PLACEHOLDER),
                            };
                        }
                        ContentPart::Image { .. } | ContentPart::Text { .. } => {}
                    }
                }
            }
            ConversationItem::ToolResult(t) => {
                t.images.retain(|part| match part {
                    ContentPart::Image { url } if should_strip(url) => {
                        stripped.push(Arc::clone(url));
                        false
                    }
                    ContentPart::Image { .. } | ContentPart::Text { .. } => true,
                });
            }
            // Exhaustive on purpose, items here and content parts above: a
            // future image-bearing variant of either must choose its strip
            // behavior here, not silently keep images.
            ConversationItem::System(_)
            | ConversationItem::Assistant(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => {}
        }
    }
    stripped
}

/// Tool choice options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationToolChoice {
    /// Model decides whether to use tools
    Auto,
    /// Model must not use tools
    None,
    /// Model must use a tool
    Required,
    /// Model must use a specific tool
    Function(String),
}

// ============================================================================
// Conversation Response
// ============================================================================

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model finished naturally
    Stop,
    /// Hit token limit
    Length,
    /// Model wants to call tools
    ToolCalls,
    /// Content was filtered
    ContentFilter,
}

impl StopReason {
    /// Stable lowercase string matching the `#[serde(rename_all = "snake_case")]` output.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Stop => "stop",
            StopReason::Length => "length",
            StopReason::ToolCalls => "tool_calls",
            StopReason::ContentFilter => "content_filter",
        }
    }
}

impl From<FinishReason> for StopReason {
    fn from(fr: FinishReason) -> Self {
        match fr {
            FinishReason::Stop => StopReason::Stop,
            FinishReason::Length => StopReason::Length,
            FinishReason::ToolCalls | FinishReason::FunctionCall => StopReason::ToolCalls,
            FinishReason::ContentFilter => StopReason::ContentFilter,
        }
    }
}

/// Token usage statistics, normalized across OpenAI Chat Completions, OpenAI Responses, and
/// Anthropic Messages backends. `prompt_tokens` is always the FULL prompt size (uncached + cache
/// reads + cache writes) and `cached_prompt_tokens` is only the cache-hit subset; do not subtract.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub reasoning_tokens: u32,
    /// Prompt tokens served from cache (the cache-hit subset of `prompt_tokens`).
    /// OpenAI: `prompt_tokens_details.cached_tokens`. Messages: `cache_read_input_tokens`.
    #[serde(default)]
    pub cached_prompt_tokens: u32,
    /// Prompt tokens written to cache this call (Messages `cache_creation_input_tokens`,
    /// billed at ~1.25x). Part of `prompt_tokens` but distinct from cache reads; 0 on
    /// backends without a cache-write signal.
    #[serde(default)]
    pub cache_creation_prompt_tokens: u32,
}

impl TokenUsage {
    pub fn record_on_span(&self, span: &tracing::Span) {
        span.record("prompt_tokens", self.prompt_tokens);
        span.record("completion_tokens", self.completion_tokens);
        span.record("reasoning_tokens", self.reasoning_tokens);
        span.record("cached_prompt_tokens", self.cached_prompt_tokens);
    }
}

impl From<Usage> for TokenUsage {
    fn from(u: Usage) -> Self {
        let cached_prompt_tokens = u
            .prompt_tokens_details
            .as_ref()
            .map_or(0, |d| d.cached_tokens);
        let total_tokens = if u.total_tokens == 0 {
            u.prompt_tokens.saturating_add(u.completion_tokens)
        } else {
            u.total_tokens
        };
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens,
            reasoning_tokens: u
                .completion_tokens_details
                .as_ref()
                .map_or(0, |d| d.reasoning_tokens),
            cached_prompt_tokens,
            cache_creation_prompt_tokens: 0,
        }
    }
}

/// Response from a conversation turn.
///
/// `items` is a flat ordered list mirroring the Responses API's
/// `output: Vec<OutputItem>`: interleaved `Reasoning`, `BackendToolCall`,
/// and a single trailing `Assistant` item (which carries the assistant text
/// and any client-executable `FunctionCall`s as `tool_calls`). Helpers
/// (`assistant()`, `empty_reason()`, etc.) treat the trailing Assistant as
/// "the response message" for backwards-compatible call sites.
#[derive(Debug, Clone)]
pub struct ConversationResponse {
    /// The flat ordered list of items produced by this turn. The trailing
    /// item is always an `Assistant` item (possibly with empty content if
    /// the model only emitted reasoning or tool calls).
    pub items: Vec<ConversationItem>,
    /// Why the model stopped generating
    pub stop_reason: Option<StopReason>,
    /// Token usage statistics
    pub usage: Option<TokenUsage>,
    /// Server cost in USD ticks (1 USD = 1e10). `None` when unreported.
    /// Capture sites must normalize with [`reported_cost_ticks`].
    pub cost_usd_ticks: Option<i64>,
    /// Number of `AgentMessageChunk` (text-only) streaming events emitted
    /// during this response.  Reasoning/thought chunks are **not** counted.
    /// When this is zero but the response contains text, the streaming
    /// events were lost (e.g. after an empty-response retry) and the caller
    /// should emit a fallback `AgentMessageChunk` so downstream consumers
    /// (e.g. the TUI) see the turn as complete.
    pub message_chunks_emitted: u64,
    /// Server-reported doom-loop triggers for this response (Responses API
    /// only, opt-in via the `x-echo-agent-doom-loop-check` header). Empty when the
    /// check is disabled or nothing was reported; deduplicated by raw label.
    /// See [`crate::doom_loop`].
    pub doom_loop_signals: Vec<crate::doom_loop::DoomLoopSignal>,
    /// Provider-supplied human-readable stop detail, when reported (e.g. a
    /// content-filter refusal explanation). Backend-neutral: normalized from
    /// the wire (Messages `message_delta.stop_details.explanation`); `None`
    /// otherwise and on backends that don't report one.
    pub stop_message: Option<String>,
    /// Provider message id (Messages `message.id`); `None` on backends that do
    /// not carry one (OAI Chat Completions / Responses).
    pub message_id: Option<String>,
    /// Verbatim wire stop reason before it collapses into [`StopReason`]
    /// (e.g. `end_turn`, `tool_use`, `pause_turn`); `None` when unreported.
    pub raw_stop_reason: Option<String>,
    /// The provider's matched stop sequence (Messages API
    /// `message_delta.stop_sequence`), present only when the model stopped on a
    /// configured stop sequence; `None` otherwise and on backends that do not
    /// report one (OAI Chat Completions / Responses).
    pub stop_sequence: Option<String>,
}

/// Normalize a wire cost-ticks value at capture.
///
/// The REST layer backfills `0` for unreported cost, and negative ticks are
/// never valid, so both become `None` ("unreported", never "free"). Every
/// ingestion path must route through this before storing
/// [`ConversationResponse::cost_usd_ticks`].
pub fn reported_cost_ticks(raw: Option<i64>) -> Option<i64> {
    raw.filter(|&t| t > 0)
}

impl ConversationResponse {
    /// The trailing `Assistant` item, if any. The producer
    /// (`response_to_conversation_items` and the streaming consumers) always
    /// appends exactly one Assistant item, but this returns `None`
    /// defensively for ad-hoc constructions in tests.
    pub fn assistant(&self) -> Option<&AssistantItem> {
        self.items.iter().rev().find_map(|item| match item {
            ConversationItem::Assistant(a) => Some(a),
            _ => None,
        })
    }

    /// Mutable view of the trailing `Assistant` item.
    pub fn assistant_mut(&mut self) -> Option<&mut AssistantItem> {
        self.items.iter_mut().rev().find_map(|item| match item {
            ConversationItem::Assistant(a) => Some(a),
            _ => None,
        })
    }

    /// Trailing assistant text content, or empty string when the response
    /// has no assistant item (or the assistant carries no text). Common
    /// shorthand for `self.assistant().map(|a| a.content.as_ref().to_owned())
    /// .unwrap_or_default()` — used by classifier / dream / summarization
    /// call sites that only care about the visible model output.
    pub fn assistant_text(&self) -> String {
        self.assistant()
            .map(|a| a.content.as_ref().to_owned())
            .unwrap_or_default()
    }

    /// Reasoning siblings that precede the trailing `Assistant`, in order.
    /// Used by streaming consumers and the empty-response retry logic that
    /// previously inspected `AssistantItem.reasoning`.
    pub fn reasoning_items(&self) -> impl Iterator<Item = &rs::ReasoningItem> {
        self.items.iter().filter_map(|item| match item {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
    }

    /// Backend-executed tool calls (web search, X search, code interpreter)
    /// produced by this turn, in emission order. These are sibling items in
    /// `items` and must also be persisted to the conversation alongside the
    /// trailing `Assistant`.
    pub fn backend_tool_items(&self) -> impl Iterator<Item = &ConversationItem> {
        self.items
            .iter()
            .filter(|item| matches!(item, ConversationItem::BackendToolCall(_)))
    }

    /// Classify why the response is empty, if it is.
    ///
    /// Returns `Some(reason)` when the response has no visible content
    /// and no tool calls (the conditions that trigger resampling).
    /// Returns `None` when the response has content or tool calls.
    pub fn empty_reason(&self) -> Option<crate::error::EmptyReason> {
        use crate::error::EmptyReason;
        let Some(a) = self.assistant() else {
            return Some(EmptyReason::NoVisibleContent);
        };
        if !a.content.is_empty() || !a.tool_calls.is_empty() {
            return None;
        }
        let has_reasoning = self
            .reasoning_items()
            .any(|r| !r.summary.is_empty() || r.content.is_some() || r.encrypted_content.is_some());
        if has_reasoning {
            Some(EmptyReason::ReasoningOnly)
        } else {
            Some(EmptyReason::NoVisibleContent)
        }
    }

    /// Check if the response is effectively empty (no content, no tool calls).
    ///
    /// Equivalent to `self.empty_reason().is_some()`. Reasoning-only
    /// responses are considered empty so the retry logic resamples.
    pub fn is_empty(&self) -> bool {
        self.empty_reason().is_some()
    }

    /// Get tool calls from the assistant message, if any
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.assistant()
            .map(|a| a.tool_calls.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the assistant text if `AgentMessageChunk` events were lost
    /// during streaming (e.g. after an empty-response retry) and a fallback
    /// emission is needed.  Returns `None` when streaming already delivered
    /// the text or when the response has no text content.
    pub fn fallback_text(&self) -> Option<String> {
        if self.message_chunks_emitted > 0 {
            return None;
        }
        let text = self
            .assistant()
            .map(|a| a.content.as_ref().to_owned())
            .unwrap_or_default();
        if text.is_empty() { None } else { Some(text) }
    }
}

// ============================================================================
// ConversationItem Constructors
// ============================================================================

impl ConversationItem {
    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self::System(SystemItem {
            content: Arc::<str>::from(content.into()),
        })
    }

    /// Create a user message with text content.
    ///
    /// `synthetic_reason` is `None` — this represents real user input.
    /// For synthetic injections, use a dedicated constructor such as
    /// [`ConversationItem::user_meta`] or [`ConversationItem::system_reminder`].
    pub fn user(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: None,
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a user message with multiple content parts.
    ///
    /// `synthetic_reason` is `None` — this represents real user input.
    pub fn user_with_parts(parts: Vec<ContentPart>) -> Self {
        Self::User(UserItem {
            content: parts,
            synthetic_reason: None,
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a synthetic user message for metadata injection.
    ///
    /// Used by the compaction pipeline to inject file contents as
    /// plain-text user messages. Tagged with [`SyntheticReason::CompactionMeta`]
    /// so downstream code (pruning, compaction helpers) skips it.
    pub fn user_meta(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::CompactionMeta),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a synthetic user message for a runtime system reminder.
    ///
    /// Used for injected `<system-reminder>` content such as skill discovery
    /// updates and plan-mode reminders. Tagged with
    /// [`SyntheticReason::SystemReminder`] so downstream code can skip it when
    /// counting real user prompts.
    pub fn system_reminder(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::SystemReminder),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Return the working-directory generation carried by this switch reminder.
    pub fn working_directory_switch_generation(&self) -> Option<u64> {
        match self {
            Self::User(user)
                if user.synthetic_reason.as_ref()
                    == Some(&SyntheticReason::WorkingDirectorySwitch) =>
            {
                user.cwd_generation
            }
            _ => None,
        }
    }

    /// User message containing project instructions (AGENTS.md / CLAUDE.md),
    /// tagged [`SyntheticReason::ProjectInstructions`] for spawn-time
    /// idempotence. Once in the conversation, MUST NOT be replaced or
    /// re-inserted — see the variant docstring.
    pub fn project_instructions(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::ProjectInstructions),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Working-directory switch reminder with a structural generation marker.
    pub fn working_directory_switch(content: impl Into<String>, cwd_generation: u64) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::WorkingDirectorySwitch),
            cwd_generation: Some(cwd_generation),
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a model-authored message received from another agent.
    pub fn agent_message(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::AgentMessage),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a synthetic user message for an auto-continue prompt.
    ///
    /// Used after compaction to tell the agent to keep working. Tagged with
    /// [`SyntheticReason::AutoContinue`] so it is not counted as a real user
    /// prompt by truncation / rewind logic.
    pub fn auto_continue(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::AutoContinue),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a synthetic user message for an auto-recovery retry prompt.
    ///
    /// Used by the auto-recovery loop after a transient tool failure. Tagged
    /// with [`SyntheticReason::AutoRecovery`] so it is not counted as a real
    /// user prompt by truncation / rewind logic.
    pub fn auto_recovery(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::AutoRecovery),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create a synthetic user message for a mid-turn interjection.
    ///
    /// Used when the user sends text via Ctrl+Enter while the model is
    /// running. Tagged with [`SyntheticReason::Interjection`] so
    /// compaction, replay, and analytics can distinguish it from real
    /// prompts and other synthetic injections.
    pub fn interjection(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::Interjection),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Auto-wake synthetic prompt for a completed background bash task.
    pub fn task_completed(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::TaskCompleted),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Auto-wake synthetic prompt for a completed background subagent.
    pub fn subagent_completed(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::SubagentCompleted),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Idle-gated notification drain (batched completions / monitor events).
    pub fn notification_drain(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::NotificationDrain),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Goal orchestrator summary turn.
    pub fn goal_summary(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::GoalSummary),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Goal-achievement classifier nudge injected after the classifier
    /// rejects an `update_goal(completed: true)` attempt. Tagged
    /// distinctly from `goal_summary` so trace tooling can tell the two
    /// synthetic user turns apart even though the wire role/tag is the
    /// same `<system-reminder>` shape.
    pub fn goal_classifier_nudge(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::GoalClassifierNudge),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Scheduled task (`/loop`) prompt fired by the scheduler.
    pub fn scheduler_fired(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::SchedulerFired),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// See [`SyntheticReason::StopHookFeedback`].
    pub fn stop_hook_feedback(content: impl Into<String>) -> Self {
        Self::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(content.into()),
            }],
            synthetic_reason: Some(SyntheticReason::StopHookFeedback),
            cwd_generation: None,
            prior_turn_interrupt: None,
            retrieval_query: None,
            knowledge_context: None,
            prompt_index: None,
        })
    }

    /// Create an assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant(AssistantItem {
            content: Arc::<str>::from(content.into()),
            tool_calls: Vec::new(),
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }

    /// Create an assistant message with a model ID.
    ///
    /// Reasoning, when present, lives as a separate sibling
    /// `ConversationItem::Reasoning(_)` placed before this item; callers
    /// should push that item to the conversation list separately.
    pub fn assistant_with_model(content: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self::Assistant(AssistantItem {
            content: Arc::<str>::from(content.into()),
            tool_calls: Vec::new(),
            model_id: Some(model_id.into()),
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }

    /// Create an assistant message that makes tool calls
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant(AssistantItem {
            content: Arc::<str>::from(""),
            tool_calls,
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }

    /// Create a tool result message
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult(ToolResultItem {
            tool_call_id: tool_call_id.into(),
            content: Arc::<str>::from(content.into()),
            images: Vec::new(),
        })
    }

    /// Create a tool result message with inline images.
    ///
    /// The images are embedded directly in the tool result message sent to
    /// the API, rather than being deferred to a follow-up user message.
    pub fn tool_result_with_images(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        images: Vec<ContentPart>,
    ) -> Self {
        Self::ToolResult(ToolResultItem {
            tool_call_id: tool_call_id.into(),
            content: Arc::<str>::from(content.into()),
            images,
        })
    }

    /// Get the role of this item
    pub fn role(&self) -> Role {
        match self {
            Self::System(_) => Role::System,
            Self::User(_) => Role::User,
            Self::Assistant(_) => Role::Assistant,
            Self::ToolResult(_) => Role::Tool,
            Self::BackendToolCall(_) => Role::Assistant,
            // Reasoning is semantically part of the assistant's turn.
            Self::Reasoning(_) => Role::Assistant,
        }
    }

    /// Add an image to a user message. No-op for other message types.
    pub fn add_image(&mut self, url: impl Into<String>) {
        if let Self::User(user) = self {
            user.content.push(ContentPart::Image {
                url: Arc::<str>::from(url.into()),
            });
        }
    }

    /// Extract text content from this item
    pub fn text_content(&self) -> String {
        match self {
            Self::System(s) => s.content.as_ref().to_owned(),
            Self::User(u) => u
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_ref()),
                    ContentPart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Assistant(a) => a.content.as_ref().to_owned(),
            Self::ToolResult(t) => t.content.as_ref().to_owned(),
            Self::BackendToolCall(b) => b.text_summary(),
            Self::Reasoning(r) => reasoning_item_text(r),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared-compaction L1 bridge: `CompactionItem` / `CompactionItemFactory`
// for `ConversationItem`
// ---------------------------------------------------------------------------
//
// Part of the EchoAgent Compaction unification. Lets the shared, transport-agnostic
// engine in `crates/common/echo-agent-compaction` operate over echo-agent-build's
// `ConversationItem` without depending on this crate — the orphan rule forces
// the impls to live here, next to the type. Mirrors the harness's
// `impl CompactionItem` for its own turn type.
//
// `CompactionItem` is the read seam (role/text/tool classification);
// `CompactionItemFactory` is the write seam the full-replace assembler
// (`apply_full_replace_compaction` / `assemble_compacted_history`) uses to
// rebuild the compacted history. Each factory constructor maps to the matching
// `ConversationItem` constructor so the `SyntheticReason` tags that the
// replay / spawn-time idempotence guards rely on are preserved.
impl echo_agent_compaction::CompactionItem for ConversationItem {
    fn role(&self) -> echo_agent_compaction::CompactionRole {
        use echo_agent_compaction::CompactionRole;
        // echo-agent-build has no distinct `Developer` role; everything maps onto
        // the four `Role` variants `ConversationItem::role()` already returns.
        match self.role() {
            Role::System => CompactionRole::System,
            Role::User => CompactionRole::User,
            Role::Assistant => CompactionRole::Assistant,
            Role::Tool => CompactionRole::Tool,
        }
    }

    fn text(&self) -> Option<String> {
        // Tool results and tool-only assistant turns can be textless; the
        // shared algorithms expect `None` rather than an empty string there.
        let text = self.text_content();
        if text.is_empty() { None } else { Some(text) }
    }

    fn has_tool_requests(&self) -> bool {
        matches!(self, Self::Assistant(a) if !a.tool_calls.is_empty())
    }

    fn is_compaction_summary(&self) -> bool {
        // echo-agent-build has no structural marker that uniquely identifies a prior
        // compaction summary: the carrier is a `user_meta` item whose
        // `SyntheticReason::CompactionMeta` is also used for re-injected file
        // contents. Returning `false` is safe for the full-replace path, which
        // does not consult this (it summarizes the whole conversation). Revisit
        // (add a dedicated marker) before routing echo-agent-build history through
        // the shared `history`/`inter` filter.
        false
    }

    fn attachment_refs(&self) -> Vec<echo_agent_compaction::CompactionFileRef> {
        // echo-agent-build `UserItem`s carry only `Text`/`Image { url }` content
        // parts — there is no id+name attachment-ref concept like the chat harness's
        // `EchoAgentTurn`. The full-replace path does not read this; revisit if
        // image attachments need to survive into the `<echo_agent_user_queries>`
        // preamble.
        Vec::new()
    }
}

impl echo_agent_compaction::CompactionItemFactory for ConversationItem {
    fn new_user(text: String) -> Self {
        Self::user(text)
    }

    fn new_user_meta(text: String) -> Self {
        Self::user_meta(text)
    }

    fn new_project_instructions(text: String) -> Self {
        Self::project_instructions(text)
    }

    fn new_system_reminder(text: String) -> Self {
        Self::system_reminder(text)
    }
}

/// Extract human-readable text from a Responses-API reasoning item by
/// joining its `summary` parts (in order) followed by its `content`
/// blocks. Both fields are optional; encrypted-only reasoning items
/// (e.g. `tco_*` backend-tool blobs) return an empty string since
/// their text is not user-visible.
///
/// Ordering contract: summary parts come first, then content blocks.
/// Streaming consumers and the Anthropic `Thinking` emitter rely on
/// this ordering to round-trip display text consistently.
pub fn reasoning_item_text(r: &rs::ReasoningItem) -> String {
    let mut parts: Vec<String> = Vec::new();
    for sp in &r.summary {
        match sp {
            rs::SummaryPart::SummaryText(t) => parts.push(t.text.clone()),
        }
    }
    if let Some(ref content) = r.content {
        for c in content {
            parts.push(c.text.clone());
        }
    }
    parts.join("\n")
}

/// Construct an `rs::ReasoningItem` carrying a single `SummaryText`
/// part — the shape every non-Responses-API streaming consumer
/// (`stream/chat_completions`, `stream/messages`, `stream/responses`
/// fallback) synthesizes when adapting a non-typed reasoning string to
/// the sibling-`Reasoning` data model.
///
/// `id` is left empty because none of the synthesizing paths carry a
/// stable upstream id; `encrypted_content` is `None` because the only
/// source of `encrypted_content` is the Responses API itself (which
/// hits the typed-`OutputItem::Reasoning` path, not this helper).
pub fn synthesized_reasoning_item(text: impl Into<String>) -> rs::ReasoningItem {
    rs::ReasoningItem {
        id: String::new(),
        summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: text.into(),
        })],
        content: None,
        encrypted_content: None,
        status: None,
    }
}

/// Splice a streaming-fallback reasoning text into a `Vec<ConversationItem>`
/// produced by `response_to_conversation_items`.
///
/// Called by `stream_responses` when the final non-streaming `Response`
/// arrives without `content` / `summary` populated but reasoning deltas
/// were observed mid-stream. Behavior:
///
/// - If any existing `Reasoning` sibling already carries text, leave
///   `items` untouched (the deltas are redundant).
/// - Otherwise, if there is a `Reasoning` sibling with no text, append
///   a `SummaryText` part to it (avoids introducing a phantom sibling).
/// - Otherwise, insert a new `Reasoning(synthesized_reasoning_item(text))`
///   immediately before the trailing `Assistant`.
pub fn inject_streaming_reasoning_fallback(items: &mut Vec<ConversationItem>, text: String) {
    if text.is_empty() {
        return;
    }
    let any_with_text = items.iter().any(|i| match i {
        ConversationItem::Reasoning(r) => r.summary.iter().any(|sp| match sp {
            rs::SummaryPart::SummaryText(t) => !t.text.is_empty(),
        }),
        _ => false,
    });
    if any_with_text {
        return;
    }
    if let Some(idx) = items
        .iter()
        .position(|i| matches!(i, ConversationItem::Reasoning(_)))
    {
        if let ConversationItem::Reasoning(r) = &mut items[idx] {
            r.summary
                .push(rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text,
                }));
        }
        return;
    }
    let pos = items
        .iter()
        .rposition(|i| matches!(i, ConversationItem::Assistant(_)))
        .unwrap_or(items.len());
    items.insert(
        pos,
        ConversationItem::Reasoning(synthesized_reasoning_item(text)),
    );
}

/// Reconstruct sibling `Reasoning` + `BackendToolCall` items from a
/// legacy chat-history row's raw JSON.
///
/// Legacy sessions stored reasoning either inline on the assistant
/// item (`AssistantItem.reasoning: Option<ReasoningContent>`) or, for
/// earlier backend-search sessions, in `AssistantItem.raw_output:
/// Option<Vec<serde_json::Value>>` (the full ordered Responses-API output
/// list including N parallel `tco_*` blobs). In the current format those fields no
/// longer exist on `AssistantItem`, so serde silently drops them on
/// deserialize. This function recovers them as sibling
/// `ConversationItem::Reasoning(_)` / `BackendToolCall(_)` items that
/// should be inserted immediately *before* the resulting assistant in the
/// loaded conversation — matching the order
/// `response_to_conversation_items` would emit if the new binary had
/// captured the original response.
///
/// Returns an empty `Vec` for non-assistant rows, already-current-format
/// rows, and rows with no recoverable reasoning data.
///
/// ## Dedup with sibling `BackendToolCall` rows
///
/// `BackendToolCall` was already a sibling variant in legacy rows, so the
/// same web-search / x-search / code-interpreter call can appear *both*
/// as a sibling line *and* inside the following assistant's
/// `raw_output`. The caller threads `sibling_btc_ids_seen` across calls
/// (updating it when it sees a `BackendToolCall` row in the JSONL stream)
/// so we don't double-emit. We also write any newly-emitted ids back
/// into the set so subsequent assistants don't re-emit.
///
/// ## Lossless fields preserved
///
/// `raw_output` path: the entry is the literal `rs::OutputItem` JSON, so
/// we round-trip it through `serde_json::from_value::<rs::OutputItem>`.
/// `id`, `summary`, `content`, `encrypted_content`, `status` all survive.
///
/// Singular `reasoning` path: builds a synthetic `rs::ReasoningItem`
/// from the legacy `ReasoningContent { text, encrypted, id }`. `id` is
/// preserved when present; Anthropic Thinking blocks never carried one
/// ([stream/messages.rs:340](crates/codegen/echo-agent-sampler/src/stream/messages.rs))
/// so the synthesized id is the empty string in that case.
///
/// v0 `ChatRequestMessage` path: top-level `reasoning_content: String`
/// becomes a single `SummaryText`-only sibling. No id / encrypted.
pub fn upgrade_legacy_reasoning(
    raw: &serde_json::Value,
    sibling_btc_ids_seen: &mut std::collections::HashSet<String>,
) -> Vec<ConversationItem> {
    let mut siblings: Vec<ConversationItem> = Vec::new();
    let Some(obj) = raw.as_object() else {
        return siblings;
    };

    // v1 assistant: type == "assistant"
    let is_v1_assistant = obj.get("type").and_then(|t| t.as_str()) == Some("assistant");
    // v0 assistant: role == "assistant" with top-level reasoning_content
    let is_v0_assistant = obj.get("role").and_then(|r| r.as_str()) == Some("assistant");

    if !is_v1_assistant && !is_v0_assistant {
        return siblings;
    }

    // Path A — v1 with `raw_output` (backend-search era). Expand each entry
    // as `rs::OutputItem` and lift Reasoning / backend-tool items to
    // siblings. `Message` / `FunctionCall` are already on the assistant
    // row (content / tool_calls), so skip them.
    if let Some(raw_output) = obj.get("raw_output").and_then(|v| v.as_array()) {
        for entry in raw_output {
            let Ok(item) = serde_json::from_value::<rs::OutputItem>(entry.clone()) else {
                continue;
            };
            match item {
                rs::OutputItem::Reasoning(r) => {
                    siblings.push(ConversationItem::Reasoning(r));
                }
                rs::OutputItem::WebSearchCall(ws) if sibling_btc_ids_seen.insert(ws.id.clone()) => {
                    siblings.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                        kind: BackendToolKind::WebSearch(ws),
                    }));
                }
                rs::OutputItem::CustomToolCall(ct)
                    if sibling_btc_ids_seen.insert(ct.id.clone()) =>
                {
                    siblings.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                        kind: BackendToolKind::XSearch(ct),
                    }));
                }
                rs::OutputItem::CodeInterpreterCall(ci)
                    if sibling_btc_ids_seen.insert(ci.id.clone()) =>
                {
                    siblings.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                        kind: BackendToolKind::CodeInterpreter(ci),
                    }));
                }
                _ => {}
            }
        }
        // raw_output is the most fidelitous source; if it was present we
        // ignore singular `reasoning` (the two are mutually exclusive in
        // practice and raw_output is the superset).
        return siblings;
    }

    // Path B — v1 with singular `reasoning: ReasoningContent` (earlier
    // clients or chat-completions written as v1).
    if is_v1_assistant && let Some(reasoning) = obj.get("reasoning").and_then(|r| r.as_object()) {
        let text = reasoning.get("text").and_then(|t| t.as_str());
        let encrypted = reasoning.get("encrypted").and_then(|t| t.as_str());
        let id = reasoning
            .get("id")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(item) = build_synthetic_reasoning(id, text, encrypted) {
            siblings.push(ConversationItem::Reasoning(item));
        }
        return siblings;
    }

    // Path C — v0 `ChatRequestMessage` with top-level `reasoning_content`.
    if is_v0_assistant
        && let Some(rc) = obj.get("reasoning_content").and_then(|t| t.as_str())
        && !rc.is_empty()
        && let Some(item) = build_synthetic_reasoning(String::new(), Some(rc), None)
    {
        siblings.push(ConversationItem::Reasoning(item));
    }

    siblings
}

/// Build a `rs::ReasoningItem` from legacy text / encrypted strings.
/// Returns `None` when there's nothing to preserve.
fn build_synthetic_reasoning(
    id: String,
    text: Option<&str>,
    encrypted: Option<&str>,
) -> Option<rs::ReasoningItem> {
    let text = text.filter(|t| !t.is_empty());
    let encrypted = encrypted.filter(|e| !e.is_empty());
    if text.is_none() && encrypted.is_none() {
        return None;
    }
    let summary = match text {
        Some(t) => vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
            text: t.to_string(),
        })],
        None => Vec::new(),
    };
    Some(rs::ReasoningItem {
        id,
        summary,
        content: None,
        encrypted_content: encrypted.map(String::from),
        status: None,
    })
}

impl UserItem {
    /// Add an image to this user message
    pub fn add_image(&mut self, url: impl Into<String>) {
        self.content.push(ContentPart::Image {
            url: Arc::<str>::from(url.into()),
        });
    }
}

impl AssistantItem {
    /// Add a tool call to this assistant message
    pub fn add_tool_call(&mut self, call: ToolCall) {
        self.tool_calls.push(call);
    }

    /// Set the model ID for this assistant message
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }
}

impl ConversationItem {
    /// Set the model ID if this is an assistant message. No-op for other types.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        if let Self::Assistant(ref mut a) = self {
            a.model_id = Some(model_id.into());
        }
        self
    }

    /// Mark this message as the genuine user turn that directly followed a
    /// user-interrupted turn (see [`PriorTurnInterrupt`]). No-op for any
    /// non-`User` variant, so callers can apply it unconditionally.
    pub fn set_prior_turn_interrupt(&mut self, interrupt: PriorTurnInterrupt) {
        if let Self::User(u) = self {
            u.prior_turn_interrupt = Some(interrupt);
        }
    }

    /// Record the prompt-turn index this user item starts (see
    /// [`UserItem::prompt_index`]). No-op for any non-`User` variant, so
    /// callers can apply it unconditionally.
    pub fn set_prompt_index(&mut self, prompt_index: usize) {
        if let Self::User(u) = self {
            u.prompt_index = Some(prompt_index);
        }
    }
}

// ============================================================================
// ConversationRequest Builder
// ============================================================================

impl ConversationRequest {
    /// Create a new empty conversation request
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from a list of conversation items
    pub fn from_items(items: Vec<ConversationItem>) -> Self {
        Self {
            items,
            ..Default::default()
        }
    }

    /// Add an item to the conversation
    pub fn push(&mut self, item: ConversationItem) {
        self.items.push(item);
    }

    /// Set the model
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set tools
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    /// Set tool choice
    pub fn with_tool_choice(mut self, choice: ConversationToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set max output tokens
    pub fn with_max_output_tokens(mut self, max_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_tokens);
        self
    }

    /// Set conversation ID header
    pub fn with_conv_id(mut self, conv_id: impl Into<String>) -> Self {
        self.x_echo_agent_conv_id = Some(conv_id.into());
        self
    }

    /// Set request ID header
    pub fn with_req_id(mut self, req_id: impl Into<String>) -> Self {
        self.x_echo_agent_req_id = Some(req_id.into());
        self
    }

    /// Set trace context for request logging.
    /// Accepts any type that implements `TraceContext` (i.e., `Clone + Send + Sync + Debug + 'static`).
    pub fn with_trace(mut self, trace: impl TraceContext + 'static) -> Self {
        self.trace = Some(Box::new(trace));
        self
    }

    pub fn with_json_schema(mut self, schema: serde_json::Value) -> Self {
        self.json_schema = Some(schema);
        self
    }
}

/// Calculate how many conversation items to keep so that everything from
/// prompt-turn `target_prompt_index` onward is dropped (the cut lands on the
/// user item that **started** that turn).
///
/// Counting is progressive:
/// - **Before the first** [`UserItem::prompt_index`]: legacy rules (first
///   marker-less non-synthetic is the `<user_info>` preamble; later
///   non-synthetics and [`SyntheticReason::starts_prompt_turn`] synthetics
///   are turns).
/// - **From the first marker onward**: only marked rows open turns; unmarked
///   mid-turn phantoms (bash / permission followup) never open a cut.
///
/// Exception: when the first marker's absolute index is **not** contiguous
/// with the unmarked prefix turn count (post-compaction rebuilds with high
/// absolute indices), fall back to pure marker mode so structural unmarked
/// rows in the rebuild prefix are not treated as historic turns.
pub fn conversation_truncate_for_prompt(
    conversation: &[ConversationItem],
    target_prompt_index: usize,
) -> usize {
    let first_marker = conversation.iter().find_map(|item| match item {
        ConversationItem::User(u) => u.prompt_index,
        _ => None,
    });

    let Some(first_marker_idx) = first_marker else {
        return conversation_truncate_legacy(conversation, target_prompt_index);
    };

    let unmarked_prefix_turns = count_legacy_turns_until_marker(conversation);
    if unmarked_prefix_turns != first_marker_idx {
        return conversation_truncate_markers_only(conversation, target_prompt_index);
    }

    conversation_truncate_progressive(conversation, target_prompt_index)
}

fn conversation_truncate_markers_only(
    conversation: &[ConversationItem],
    target_prompt_index: usize,
) -> usize {
    for (i, item) in conversation.iter().enumerate() {
        let ConversationItem::User(user) = item else {
            continue;
        };
        if let Some(idx) = user.prompt_index
            && idx >= target_prompt_index
        {
            return i;
        }
    }
    conversation.len()
}

fn conversation_truncate_legacy(
    conversation: &[ConversationItem],
    target_prompt_index: usize,
) -> usize {
    let mut next_unmarked_index = 0usize;
    let mut seen_unmarked_preamble = false;

    for (i, item) in conversation.iter().enumerate() {
        let ConversationItem::User(user) = item else {
            continue;
        };

        let effective_index = match &user.synthetic_reason {
            None if !seen_unmarked_preamble => {
                seen_unmarked_preamble = true;
                None
            }
            None => Some(next_unmarked_index),
            Some(reason) if reason.starts_prompt_turn() => Some(next_unmarked_index),
            Some(_) => None,
        };

        if let Some(idx) = effective_index {
            if idx >= target_prompt_index {
                return i;
            }
            next_unmarked_index = idx + 1;
        }
    }

    conversation.len()
}

/// Count legacy turns in the unmarked prefix (stops at the first marker).
fn count_legacy_turns_until_marker(conversation: &[ConversationItem]) -> usize {
    let mut turns = 0usize;
    let mut seen_unmarked_preamble = false;

    for item in conversation {
        let ConversationItem::User(user) = item else {
            continue;
        };
        if user.prompt_index.is_some() {
            break;
        }
        match &user.synthetic_reason {
            None if !seen_unmarked_preamble => {
                seen_unmarked_preamble = true;
            }
            None => turns += 1,
            Some(reason) if reason.starts_prompt_turn() => turns += 1,
            Some(_) => {}
        }
    }
    turns
}

fn conversation_truncate_progressive(
    conversation: &[ConversationItem],
    target_prompt_index: usize,
) -> usize {
    let mut next_unmarked_index = 0usize;
    let mut seen_unmarked_preamble = false;
    let mut seen_marker = false;

    for (i, item) in conversation.iter().enumerate() {
        let ConversationItem::User(user) = item else {
            continue;
        };

        let effective_index = if let Some(idx) = user.prompt_index {
            seen_marker = true;
            Some(idx)
        } else if seen_marker {
            None
        } else {
            match &user.synthetic_reason {
                None if !seen_unmarked_preamble => {
                    seen_unmarked_preamble = true;
                    None
                }
                None => Some(next_unmarked_index),
                Some(reason) if reason.starts_prompt_turn() => Some(next_unmarked_index),
                Some(_) => None,
            }
        };

        if let Some(idx) = effective_index {
            if idx >= target_prompt_index {
                return i;
            }
            if user.prompt_index.is_none() {
                next_unmarked_index = idx + 1;
            }
        }
    }

    conversation.len()
}

/// Transform CWD paths in conversation items (used for forked sessions).
/// This replaces source_cwd with target_cwd in text content.
pub fn transform_conversation_cwd(
    items: &mut [ConversationItem],
    source_cwd: &str,
    target_cwd: &str,
) {
    for item in items.iter_mut() {
        match item {
            ConversationItem::System(s) => {
                if s.content.contains(source_cwd) {
                    s.content = Arc::<str>::from(s.content.replace(source_cwd, target_cwd));
                }
            }
            ConversationItem::User(u) => {
                for part in u.content.iter_mut() {
                    if let ContentPart::Text { text } = part
                        && text.contains(source_cwd)
                    {
                        *text = Arc::<str>::from(text.replace(source_cwd, target_cwd));
                    }
                }
            }
            ConversationItem::Assistant(a) => {
                if a.content.contains(source_cwd) {
                    a.content = Arc::<str>::from(a.content.replace(source_cwd, target_cwd));
                }
                // Tool call arguments contain file paths that must also be rewritten.
                // The arguments field is a JSON-encoded string; source_cwd appears as
                // a literal substring (serde_json does not escape `/`), so str::replace
                // is safe. This is needed in both directions:
                //   Forward (root → worktree): so the fork session's history uses worktree paths.
                //   Reverse (worktree → root): so the synced-back session doesn't reference
                //     a deleted worktree directory on the next turn.
                for tc in &mut a.tool_calls {
                    if tc.arguments.contains(source_cwd) {
                        tc.arguments =
                            Arc::<str>::from(tc.arguments.replace(source_cwd, target_cwd));
                    }
                }
            }
            ConversationItem::ToolResult(t) => {
                if t.content.contains(source_cwd) {
                    t.content = Arc::<str>::from(t.content.replace(source_cwd, target_cwd));
                }
            }
            // Backend tool calls don't contain workspace paths — no-op.
            ConversationItem::BackendToolCall(_) => {}
            // Reasoning items rarely reference CWD paths, but they can —
            // patch both summary parts and content blocks defensively.
            ConversationItem::Reasoning(r) => {
                for sp in r.summary.iter_mut() {
                    match sp {
                        rs::SummaryPart::SummaryText(t) => {
                            if t.text.contains(source_cwd) {
                                t.text = t.text.replace(source_cwd, target_cwd);
                            }
                        }
                    }
                }
                if let Some(ref mut content) = r.content {
                    for c in content.iter_mut() {
                        if c.text.contains(source_cwd) {
                            c.text = c.text.replace(source_cwd, target_cwd);
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Conversation Repair
// ============================================================================

/// Why a tool call ended up dangling — controls the synthetic-result wording.
///
/// Each variant maps to a distinct synthetic `ToolResult` body produced by
/// [`repair_dangling_tool_calls`]. The wording is model-actionable: the
/// model needs to know whether to retry, switch strategy, or treat the
/// failure as terminal.
///
/// A `PostProcessingFailed` variant existed in an earlier revision for
/// a mid-turn tool post-processing error path. It is now intentionally
/// absent because that path no longer returns `Err` between
/// `tool_completed` and `push_tool_result` — every error mode is
/// degraded inline. The variant would have shipped without any
/// production producer. If a future error path between
/// `tool_completed` and `push_tool_result` is added, add a fresh
/// variant here so [`synthetic_dangling_result_text`]'s exhaustive
/// match flags every renderer.
#[derive(Debug, Clone, Copy)]
pub enum DanglingToolCallReason {
    /// User pressed Ctrl+C / aborted, or the cause cannot be determined.
    ///
    /// Default fallback when no more specific reason is plumbed through.
    UserCancelled,
    /// Harness halted the turn (internal error, policy guard, etc.).
    ///
    /// `class` is a stable taxonomy tag used by metrics and the synthetic
    /// message; it is `&'static str` because every call site is known at
    /// compile time.
    HarnessHalted { class: &'static str },
}

/// Insert synthetic `ToolResult` items for any tool calls that lack a result.
///
/// When a turn is cancelled mid-tool-execution, the conversation can have an
/// assistant message with `tool_calls` but no matching `ToolResult`. The API
/// rejects this with "No tool output found for function call …".
///
/// Scans the entire conversation front-to-back. For every assistant message
/// that has `tool_calls`, it checks which calls are answered by the
/// immediately following `ToolResult` items and inserts synthetic results
/// for any that are missing, preserving the original call order. `reason`
/// controls the wording of those synthetic results.
///
/// A full scan is necessary because old sessions (or sessions that switched
/// API providers) may have dangling tool calls anywhere in the history, not
/// just at the tail.
///
/// Returns the number of synthetic tool results inserted.
pub fn repair_dangling_tool_calls(
    conversation: &mut Vec<ConversationItem>,
    reason: DanglingToolCallReason,
) -> usize {
    // Phase 1: forward scan to find every assistant with unanswered tool calls.
    // We record (insert_position, synthetic_items) for each repair site.
    let mut repairs: Vec<(usize, Vec<ConversationItem>)> = Vec::new();
    let mut i = 0;

    while i < conversation.len() {
        if let ConversationItem::Assistant(a) = &conversation[i]
            && !a.tool_calls.is_empty()
        {
            // Snapshot the call metadata we need (avoids borrowing `conversation`).
            let tool_calls: Vec<(Arc<str>, String)> = a
                .tool_calls
                .iter()
                .map(|tc| (tc.id.clone(), tc.name.clone()))
                .collect();

            // Collect answered IDs from the immediately following ToolResult items.
            let mut answered = std::collections::HashSet::new();
            let mut j = i + 1;
            while j < conversation.len() {
                if let ConversationItem::ToolResult(tr) = &conversation[j] {
                    answered.insert(tr.tool_call_id.clone());
                    j += 1;
                } else {
                    break;
                }
            }

            // Build synthetic results for unanswered calls, preserving call order.
            let synthetic: Vec<ConversationItem> = tool_calls
                .iter()
                .filter(|(id, _)| !answered.contains(id.as_ref()))
                .map(|(id, name)| {
                    ConversationItem::tool_result(
                        id.as_ref(),
                        synthetic_dangling_result_text(name, reason),
                    )
                })
                .collect();

            if !synthetic.is_empty() {
                repairs.push((j, synthetic));
            }

            i = j;
            continue;
        }
        i += 1;
    }

    // Phase 2: apply repairs in reverse index order so earlier indices stay valid.
    let total: usize = repairs.iter().map(|(_, s)| s.len()).sum();
    for (insert_at, synthetic) in repairs.into_iter().rev() {
        conversation.splice(insert_at..insert_at, synthetic);
    }
    total
}

/// Read-only counterpart to [`repair_dangling_tool_calls`]: returns `true` if
/// any assistant message has a tool call that is not answered by a `ToolResult`
/// in the immediately-following run of results.
///
/// Lets callers decide whether the repair *would* fire (and therefore already
/// signal a cancellation to the model) without mutating the conversation. Uses
/// the same forward-scan / immediately-following-results logic as
/// [`repair_dangling_tool_calls`], short-circuiting on the first unanswered call.
pub fn has_dangling_tool_calls(conversation: &[ConversationItem]) -> bool {
    let mut i = 0;
    while i < conversation.len() {
        if let ConversationItem::Assistant(a) = &conversation[i]
            && !a.tool_calls.is_empty()
        {
            // Collect answered IDs from the immediately following ToolResults.
            let mut answered = std::collections::HashSet::new();
            let mut j = i + 1;
            while j < conversation.len() {
                if let ConversationItem::ToolResult(tr) = &conversation[j] {
                    answered.insert(tr.tool_call_id.clone());
                    j += 1;
                } else {
                    break;
                }
            }
            if a.tool_calls
                .iter()
                .any(|tc| !answered.contains(tc.id.as_ref()))
            {
                return true;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    false
}

fn synthetic_dangling_result_text(name: &str, reason: DanglingToolCallReason) -> String {
    // Exhaustive match — no `_ =>` guard — so adding a new variant is a
    // compile-time error at every call site that renders these messages.
    match reason {
        DanglingToolCallReason::UserCancelled => {
            format!("Tool execution was cancelled by the user (tool `{name}` was not executed).")
        }
        DanglingToolCallReason::HarnessHalted { class } => format!(
            "Tool execution was halted by the harness ({class}); the tool `{name}` was not executed.",
        ),
    }
}

/// Remove duplicate `ToolResult` entries for the same `tool_call_id`.
///
/// When a tool call is cancelled (e.g. Ctrl-C or crash) and then later the
/// real result also arrives, the conversation can end up with two `ToolResult`
/// entries sharing the same `tool_call_id`.  The LLM API rejects this with
/// "each tool_use must have a single result".
///
/// This function scans the `ToolResult` items immediately following each
/// assistant message.  If a `tool_call_id` appears more than once, only the
/// **last** occurrence is kept (the real result), and earlier duplicates are
/// removed.
///
/// Returns the number of duplicate entries removed.
pub fn dedup_duplicate_tool_results(conversation: &mut Vec<ConversationItem>) -> usize {
    let mut total_removed = 0;
    let mut i = 0;

    while i < conversation.len() {
        // Look for assistant messages with tool calls.
        if let ConversationItem::Assistant(a) = &conversation[i]
            && !a.tool_calls.is_empty()
        {
            // Scan the run of ToolResult items immediately after.
            let start = i + 1;
            let mut end = start;
            while end < conversation.len() {
                if matches!(&conversation[end], ConversationItem::ToolResult(_)) {
                    end += 1;
                } else {
                    break;
                }
            }

            // Within [start..end), find duplicates by tool_call_id.
            // Keep the *last* occurrence of each id (the real result).
            if end > start {
                let mut seen = std::collections::HashMap::<String, usize>::new();
                let mut to_remove = Vec::<usize>::new();

                for (idx, item) in conversation.iter().enumerate().take(end).skip(start) {
                    if let ConversationItem::ToolResult(tr) = item
                        && let Some(prev) = seen.insert(tr.tool_call_id.clone(), idx)
                    {
                        // We've seen this id before — mark the *previous* for removal.
                        to_remove.push(prev);
                    }
                }

                if !to_remove.is_empty() {
                    // Remove in reverse order so indices stay valid.
                    to_remove.sort_unstable();
                    to_remove.reverse();
                    for idx in &to_remove {
                        conversation.remove(*idx);
                    }
                    total_removed += to_remove.len();
                    // Don't advance i — the window shifted, re-scan from same spot.
                    continue;
                }
            }

            i = end;
            continue;
        }
        i += 1;
    }

    total_removed
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod compaction_item_bridge_tests {
    use super::*;
    use echo_agent_compaction::{CompactionItem, CompactionItemFactory, CompactionRole};

    #[test]
    fn role_maps_every_variant() {
        assert_eq!(
            CompactionItem::role(&ConversationItem::system("s")),
            CompactionRole::System
        );
        assert_eq!(
            CompactionItem::role(&ConversationItem::user("u")),
            CompactionRole::User
        );
        assert_eq!(
            CompactionItem::role(&ConversationItem::assistant("a")),
            CompactionRole::Assistant
        );
        assert_eq!(
            CompactionItem::role(&ConversationItem::tool_result("tc1", "r")),
            CompactionRole::Tool
        );
        // BackendToolCall / Reasoning are semantically part of the assistant
        // turn — they must map to Assistant, never Tool.
        assert_eq!(
            CompactionItem::role(&ConversationItem::Reasoning(synthesized_reasoning_item(
                "t"
            ))),
            CompactionRole::Assistant
        );
    }

    #[test]
    fn text_is_none_when_empty_some_otherwise() {
        assert_eq!(
            CompactionItem::text(&ConversationItem::user("hello")),
            Some("hello".to_string())
        );
        // An assistant tool-only turn has empty text content -> None.
        let tool_only = ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "tc1".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        assert_eq!(CompactionItem::text(&tool_only), None);
    }

    #[test]
    fn has_tool_requests_only_for_assistant_with_tool_calls() {
        let with_tools = ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "tc1".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        assert!(CompactionItem::has_tool_requests(&with_tools));
        assert!(!CompactionItem::has_tool_requests(
            &ConversationItem::assistant("no tools")
        ));
        assert!(!CompactionItem::has_tool_requests(&ConversationItem::user(
            "u"
        )));
        assert!(!CompactionItem::has_tool_requests(
            &ConversationItem::tool_result("tc1", "r")
        ));
    }

    #[test]
    fn is_tool_result_default_tracks_role() {
        assert!(CompactionItem::is_tool_result(
            &ConversationItem::tool_result("tc1", "r")
        ));
        assert!(!CompactionItem::is_tool_result(&ConversationItem::user(
            "u"
        )));
    }

    #[test]
    fn metadata_accessors_are_conservative() {
        // echo-agent-build has no structural compaction-summary marker, and no
        // id+name attachment refs, so both return empty/false.
        assert!(!CompactionItem::is_compaction_summary(
            &ConversationItem::user("u")
        ));
        assert!(!CompactionItem::is_compaction_summary(
            &ConversationItem::user_meta("summary")
        ));
        assert!(CompactionItem::attachment_refs(&ConversationItem::user("u")).is_empty());
    }

    /// The write seam must map each constructor to the matching
    /// `ConversationItem` with the `SyntheticReason` tag the replay /
    /// spawn-time idempotence guards rely on, so a compacted history rebuilt
    /// through the shared assembler is indistinguishable from the in-shell one.
    #[test]
    fn factory_constructors_preserve_synthetic_reason_tags() {
        let plain = <ConversationItem as CompactionItemFactory>::new_user("q".into());
        assert_matches_user_reason(&plain, None);

        let meta = <ConversationItem as CompactionItemFactory>::new_user_meta("m".into());
        assert_matches_user_reason(&meta, Some(SyntheticReason::CompactionMeta));

        let proj =
            <ConversationItem as CompactionItemFactory>::new_project_instructions("p".into());
        assert_matches_user_reason(&proj, Some(SyntheticReason::ProjectInstructions));

        let reminder = <ConversationItem as CompactionItemFactory>::new_system_reminder("r".into());
        assert_matches_user_reason(&reminder, Some(SyntheticReason::SystemReminder));
    }

    fn assert_matches_user_reason(item: &ConversationItem, expected: Option<SyntheticReason>) {
        let ConversationItem::User(parts) = item else {
            panic!("factory must produce a User item, got {item:?}");
        };
        assert_eq!(parts.synthetic_reason, expected);
    }
}

#[cfg(test)]
#[path = "conversation/test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "conversation/chat_completions_tests.rs"]
mod chat_completions_tests;

#[cfg(test)]
#[path = "conversation/responses_tests.rs"]
mod responses_tests;

#[cfg(test)]
#[path = "conversation/messages_tests.rs"]
mod messages_tests;

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::tool_overrides::*;
    use assert_matches::assert_matches;

    /// Keeps `forwards_prompt_cache_key()` honest against each mapping: a key that never reaches the wire looks like a 0% cache hit, not a bug.
    #[test]
    fn prompt_cache_key_reaches_the_wire_only_where_the_backend_claims() {
        let request = || ConversationRequest {
            items: vec![ConversationItem::user("hi")],
            model: Some("test-model".to_string()),
            prompt_cache_key: Some("cache-key-1".to_string()),
            ..Default::default()
        };

        for backend in [
            crate::ApiBackend::ChatCompletions,
            crate::ApiBackend::Responses,
            crate::ApiBackend::Messages,
        ] {
            let on_wire = match backend {
                crate::ApiBackend::Responses => {
                    rs::CreateResponse::from(&request())
                        .prompt_cache_key
                        .as_deref()
                        == Some("cache-key-1")
                }
                crate::ApiBackend::ChatCompletions => {
                    let mapped = ChatCompletionRequest::from(request());
                    serde_json::to_value(&mapped)
                        .expect("chat request serializes")
                        .get("prompt_cache_key")
                        .is_some()
                }
                crate::ApiBackend::Messages => {
                    let mapped = super::messages::build_messages_request(&request());
                    serde_json::to_value(&mapped)
                        .expect("messages request serializes")
                        .get("prompt_cache_key")
                        .is_some()
                }
            };
            assert_eq!(
                on_wire,
                backend.forwards_prompt_cache_key(),
                "{backend:?}: forwards_prompt_cache_key() disagrees with the mapping"
            );
        }
    }

    #[test]
    fn prior_turn_interrupt_serde_round_trip_and_unknown_fallback() {
        for (variant, wire) in [
            (PriorTurnInterrupt::MidTurnAbort, "\"mid_turn_abort\""),
            (
                PriorTurnInterrupt::PermissionRejected,
                "\"permission_rejected\"",
            ),
            (
                PriorTurnInterrupt::PermissionCancelled,
                "\"permission_cancelled\"",
            ),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
            let back: PriorTurnInterrupt = serde_json::from_str(wire).unwrap();
            assert_eq!(back, variant);
        }
        // Forward-compat: an unknown wire string decodes to `Unknown`.
        let unknown: PriorTurnInterrupt = serde_json::from_str("\"some_future_cause\"").unwrap();
        assert_eq!(unknown, PriorTurnInterrupt::Unknown);
    }

    #[test]
    fn user_item_prior_turn_interrupt_omitted_when_none_present_when_set() {
        // A real prompt with no marker omits the field entirely (byte-stable
        // with sessions written before this field existed).
        let plain = ConversationItem::user("hello");
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("prior_turn_interrupt"),
            "None marker must be omitted, got {json}"
        );

        // The setter stamps it; it serializes and round-trips.
        let mut marked = ConversationItem::user("hello");
        marked.set_prior_turn_interrupt(PriorTurnInterrupt::MidTurnAbort);
        let json = serde_json::to_string(&marked).unwrap();
        assert!(
            json.contains("\"prior_turn_interrupt\":\"mid_turn_abort\""),
            "got {json}"
        );
        let back: ConversationItem = serde_json::from_str(&json).unwrap();
        assert_matches!(
            back,
            ConversationItem::User(UserItem {
                prior_turn_interrupt: Some(PriorTurnInterrupt::MidTurnAbort),
                ..
            })
        );
    }

    #[test]
    fn set_prior_turn_interrupt_is_noop_on_non_user_items() {
        let mut sys = ConversationItem::system("sp");
        sys.set_prior_turn_interrupt(PriorTurnInterrupt::MidTurnAbort);
        assert_matches!(sys, ConversationItem::System(_));
    }

    #[test]
    fn tool_overrides_update_apply_merges_tristate() {
        let x = XSearchOptions {
            date_bound: Some(SearchDateBound::new(None, Some("2024-03-15".into())).unwrap()),
        };
        let w = WebSearchOptions {
            allowed_domains: Some(vec!["x.com".into()]),
            excluded_domains: None,
        };

        // set: an object sets that tool's options.
        let base = ToolOverridesUpdate {
            x_search: Some(Some(x.clone())),
            web_search: None,
        }
        .apply(None);
        assert_eq!(
            base.as_ref().and_then(|o| o.x_search.clone()),
            Some(x.clone())
        );

        // leave: an absent field keeps the base's entry; a set field updates only itself.
        let merged = ToolOverridesUpdate {
            x_search: None,
            web_search: Some(Some(w.clone())),
        }
        .apply(base.clone());
        assert_eq!(merged.as_ref().and_then(|o| o.x_search.clone()), Some(x));
        assert_eq!(merged.and_then(|o| o.web_search), Some(w));

        // clear: `null` clears just that tool; clearing the last remaining tool
        // empties the override to `None`.
        let cleared = ToolOverridesUpdate {
            x_search: Some(None),
            web_search: None,
        }
        .apply(base);
        assert!(cleared.is_none());
    }

    #[test]
    fn empty_per_turn_override_never_clears_a_seeded_cutoff() {
        use serde_json::json;
        // A stray empty `{}` carries no instruction, so a definition-seeded cutoff must survive it
        // (only an explicit bound changes the window; `null` reverts to the seed).
        let update = ToolOverridesUpdate::parse(&json!({"xSearch": {}}))
            .unwrap()
            .apply(None);
        let mut tools = vec![HostedTool::XSearch {
            options: Some(XSearchOptions {
                date_bound: Some(SearchDateBound::new(None, Some("2024-01-01".into())).unwrap()),
            }),
        }];
        let applied = apply_tool_overrides(&mut tools, update.as_ref());
        assert_eq!(
            applied
                .x_search
                .and_then(|x| x.date_bound)
                .and_then(|b| b.to_date().map(str::to_owned)),
            Some("2024-01-01".to_string()),
            "an empty override must not widen a seeded cutoff"
        );

        let mut tools = vec![HostedTool::XSearch {
            options: Some(XSearchOptions {
                date_bound: Some(SearchDateBound::new(None, Some("2024-01-01".into())).unwrap()),
            }),
        }];
        let direct = ToolOverrides::parse(&json!({"xSearch": {}})).unwrap();
        let applied = apply_tool_overrides(&mut tools, Some(&direct));
        assert_eq!(
            applied
                .x_search
                .and_then(|x| x.date_bound)
                .and_then(|b| b.to_date().map(str::to_owned)),
            Some("2024-01-01".to_string()),
            "an empty override leaves the seeded bound, which stays attested"
        );
    }

    #[test]
    fn search_date_bound_validation() {
        // Non-canonical dates: unpadded is NotZeroPadded; a five-digit year and year 0 (below the
        // minimum year 1) are InvalidDate; a valid padded window is accepted.
        assert!(matches!(
            SearchDateBound::new(Some("2024-3-5".into()), None),
            Err(SearchDateBoundError::NotZeroPadded { .. })
        ));
        assert!(matches!(
            SearchDateBound::new(Some("10000-01-01".into()), None),
            Err(SearchDateBoundError::InvalidDate { .. })
        ));
        assert!(matches!(
            SearchDateBound::new(Some("0000-01-01".into()), None),
            Err(SearchDateBoundError::InvalidDate { .. })
        ));
        assert!(SearchDateBound::new(Some("0001-01-01".into()), Some("0099-12-31".into())).is_ok());

        // Inverted window is rejected with the typed error; equal and ordered windows are accepted.
        assert!(matches!(
            SearchDateBound::new(Some("2024-03-15".into()), Some("2024-01-01".into())),
            Err(SearchDateBoundError::InvertedWindow { .. })
        ));
        assert!(SearchDateBound::new(Some("2024-01-01".into()), Some("2024-01-01".into())).is_ok());
        assert!(SearchDateBound::new(Some("2024-01-01".into()), Some("2024-01-02".into())).is_ok());

        // The rejection also holds through parse and the composed aggregate wire type, so a client
        // cannot smuggle an inverted window past the outer types.
        let inverted = serde_json::json!({"fromDate": "2024-03-15", "toDate": "2024-01-01"});
        let err = SearchDateBound::parse(&inverted)
            .expect_err("inverted window must fail parse")
            .to_string();
        assert!(err.contains("on or before"), "unhelpful error: {err}");
        assert!(
            ToolOverridesUpdate::parse(&serde_json::json!({"xSearch": {"dateBound": &inverted}}))
                .is_err(),
            "inverted window must fail through the aggregate wire type"
        );
    }

    #[test]
    fn json_schema_converts_to_all_three_api_wire_formats() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"]
        });

        let req = ConversationRequest::from_items(vec![ConversationItem::user("Summarize")])
            .with_json_schema(schema.clone());

        // Chat Completions: json_schema → response_format
        let chat_req: ChatCompletionRequest = req.clone().into();
        let fmt = serde_json::to_value(chat_req.response_format.unwrap()).unwrap();
        assert_eq!(fmt["type"], "json_schema");
        assert_eq!(fmt["json_schema"]["name"], STRUCTURED_OUTPUT_SCHEMA_NAME);
        assert_eq!(fmt["json_schema"]["strict"], true);
        assert_eq!(fmt["json_schema"]["schema"], schema);

        // Responses API: json_schema → text.format
        let resp: rs::CreateResponse = (&req).into();
        let rs::TextResponseFormatConfiguration::JsonSchema(f) = resp.text.unwrap().format else {
            panic!("Expected json_schema format");
        };
        assert_eq!(f.name, STRUCTURED_OUTPUT_SCHEMA_NAME);
        assert_eq!(f.strict, Some(true));
        assert_eq!(f.schema, Some(schema.clone()));

        // Messages API: json_schema → output_config.format
        let msgs_req = build_messages_request(&req);
        let output_config = msgs_req.output_config.expect("output_config should be set");
        let fmt = output_config.format.expect("format should be set");
        let crate::messages::OutputFormat::JsonSchema { schema: s } = fmt;
        assert_eq!(s, schema);
        assert!(msgs_req.thinking.is_none());
        assert!(output_config.effort.is_none());
    }

    // ============================================================================
    // Encrypted Reasoning Tests
    // ============================================================================

    #[test]
    fn test_reasoning_content_from_text() {
        let reasoning = ReasoningContent::from_text("Let me think step by step...");
        assert_eq!(
            reasoning.text.as_deref(),
            Some("Let me think step by step...")
        );
        assert!(reasoning.encrypted.is_none());
        assert!(!reasoning.is_empty());
    }

    #[test]
    fn test_reasoning_content_from_encrypted() {
        let reasoning = ReasoningContent::from_encrypted("enc_abc123_encrypted_data");
        assert!(reasoning.text.is_none());
        assert_eq!(
            reasoning.encrypted.as_deref(),
            Some("enc_abc123_encrypted_data")
        );
        assert!(!reasoning.is_empty());
    }

    #[test]
    fn test_reasoning_content_helper_round_trip() {
        // `ReasoningContent` still exists for the chat-completions wire +
        // legacy paths. Confirm both fields survive serde.
        let reasoning = ReasoningContent {
            text: Some("Visible reasoning".into()),
            encrypted: Some("enc_hidden_data".into()),
            id: None,
        };
        assert!(!reasoning.is_empty());
        let json = serde_json::to_string(&reasoning).expect("serialize");
        let back: ReasoningContent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.text.as_deref(), Some("Visible reasoning"));
        assert_eq!(back.encrypted.as_deref(), Some("enc_hidden_data"));
    }

    #[test]
    fn test_reasoning_content_empty() {
        let reasoning = ReasoningContent {
            text: None,
            encrypted: None,
            id: None,
        };
        assert!(reasoning.is_empty());
    }

    #[test]
    fn test_reasoning_content_serialization_with_encrypted() {
        // Test that ReasoningContent correctly serializes/deserializes with both fields
        let reasoning = ReasoningContent {
            text: Some("Let me think...".into()),
            encrypted: Some("enc_abc123".into()),
            id: None,
        };

        let json = serde_json::to_string(&reasoning).expect("Should serialize");
        assert!(json.contains("Let me think..."));
        assert!(json.contains("enc_abc123"));

        let back: ReasoningContent = serde_json::from_str(&json).expect("Should deserialize");
        assert_eq!(back.text.as_deref(), Some("Let me think..."));
        assert_eq!(back.encrypted.as_deref(), Some("enc_abc123"));
    }

    #[test]
    fn test_tool_definition_from_tool_spec() {
        let spec = ToolSpec {
            name: "my_tool".to_string(),
            description: Some("Does something".to_string()),
            parameters: serde_json::json!({"type": "object"}),
        };

        let def = ToolDefinition::function(
            spec.name.clone(),
            spec.description.clone(),
            spec.parameters.clone(),
        );

        assert_eq!(def.function.name, "my_tool");
        assert_eq!(def.function.description, Some("Does something".to_string()));
    }

    // ============================================================================
    // Edge Cases Tests
    // ============================================================================

    #[test]
    fn test_empty_content() {
        // Empty user message
        let user = ConversationItem::user("");
        assert_eq!(user.text_content(), "");

        // Empty assistant message
        let assistant = ConversationItem::assistant("");
        assert_eq!(assistant.text_content(), "");

        // Empty system message
        let system = ConversationItem::system("");
        assert_eq!(system.text_content(), "");
    }

    #[test]
    fn test_empty_tool_calls() {
        let assistant = ConversationItem::assistant_tool_calls(vec![]);
        let ConversationItem::Assistant(a) = assistant else {
            panic!("Expected Assistant item");
        };
        assert!(a.tool_calls.is_empty());
        assert!(a.content.is_empty());
    }

    #[test]
    fn test_user_with_only_image() {
        let parts = vec![ContentPart::Image {
            url: "https://example.com/image.png".into(),
        }];

        let user = ConversationItem::user_with_parts(parts);
        assert_eq!(user.text_content(), ""); // No text content
    }

    #[test]
    fn test_messages_request_cache_breakpoint_placement() {
        let json = agent_request(2);
        let messages = json["messages"].as_array().unwrap();

        assert_eq!(
            json.pointer("/system/0/cache_control/type")
                .and_then(|v| v.as_str()),
            Some("ephemeral"),
            "{json:#}",
        );
        assert_eq!(
            marker_on_last_block(messages.last().unwrap()),
            Some("ephemeral"),
            "tip: {json:#}"
        );
        assert_eq!(
            messages.last().unwrap()["content"]
                .as_array()
                .and_then(|b| b.last())
                .and_then(|b| b["type"].as_str()),
            Some("tool_result"),
        );

        let previous_user = messages[..messages.len() - 1]
            .iter()
            .rposition(|m| m["role"] == "user")
            .unwrap();
        assert_eq!(
            marker_on_last_block(&messages[previous_user]),
            Some("ephemeral"),
            "previous turn's tip: {json:#}",
        );
        assert_eq!(count_cache_control(&json), 3, "{json:#}");
    }

    /// Same truncation pattern should also strip trailing ToolResult items
    /// that appear without their owning assistant (edge case from partial
    /// conversation state).
    #[test]
    fn test_btw_mid_turn_truncation_strips_partial_tool_result_run() {
        let mut items = vec![
            ConversationItem::user("hello"),
            ConversationItem::assistant("I'll search."),
            ConversationItem::assistant_tool_calls(vec![
                ToolCall {
                    id: "call_A".into(),
                    name: "grep".to_string(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call_B".into(),
                    name: "read_file".to_string(),
                    arguments: "{}".into(),
                },
            ]),
            // Only one of two tool results arrived
            ConversationItem::tool_result("call_A", "match found"),
        ];

        while let Some(last) = items.last() {
            match last {
                ConversationItem::Assistant(a) if !a.tool_calls.is_empty() => {
                    items.pop();
                }
                ConversationItem::ToolResult(_) => {
                    items.pop();
                }
                _ => break,
            }
        }

        // The trailing tool_result and the assistant with tool_calls should
        // both be removed, leaving just user + assistant text.
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], ConversationItem::User(_)));
        assert!(matches!(items[1], ConversationItem::Assistant(_)));
    }

    /// truncate_bytes must not panic on a multi-byte char boundary.
    #[test]
    fn test_truncate_bytes_non_ascii() {
        // "路径" is 6 bytes (2 CJK chars × 3 bytes each).
        // Truncating at 4 would land inside the second char — must walk back to 3.
        let s = "路径";
        assert_eq!(s.len(), 6);
        assert_eq!(truncate_bytes(s, 4), "路"); // only 3 bytes fit
        assert_eq!(truncate_bytes(s, 3), "路"); // exact boundary
        assert_eq!(truncate_bytes(s, 6), s); // full string
        assert_eq!(truncate_bytes(s, 100), s); // larger than string
        assert_eq!(truncate_bytes(s, 0), ""); // zero

        // Emoji (4-byte): truncating at 5 must back up to 4.
        let e = "🎉!";
        assert_eq!(e.len(), 5); // 4 + 1
        assert_eq!(truncate_bytes(e, 5), "🎉!");
        assert_eq!(truncate_bytes(e, 4), "🎉");
        assert_eq!(truncate_bytes(e, 3), ""); // 3 < 4, walks back to 0
    }

    // ============================================================================
    // conversation_truncate_for_prompt Tests
    // ============================================================================

    #[test]
    fn test_truncate_for_prompt_basic() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"), // prompt 0
            ConversationItem::assistant("Asst 1"),
            ConversationItem::user("User 2"), // prompt 1
            ConversationItem::assistant("Asst 2"),
            ConversationItem::user("User 3"), // prompt 2
        ];

        // Keep up to and including prompt 0 (first user message)
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 3);

        // Keep up to and including prompt 1
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 5);

        // Keep up to and including prompt 2 (all messages)
        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 6);
    }

    #[test]
    fn test_truncate_for_prompt_with_tool_calls() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"), // prompt 0
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "call_1".into(),
                name: "bash".to_string(),
                arguments: "{}".into(),
            }]),
            ConversationItem::tool_result("call_1", "result"),
            ConversationItem::assistant("Done"),
            ConversationItem::user("User 2"), // prompt 1
        ];

        // Keep up to prompt 0
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 5);

        // Keep up to prompt 1 (all)
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 6);
    }

    #[test]
    fn test_truncate_for_prompt_empty() {
        let conversation: Vec<ConversationItem> = vec![];
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 0);
    }

    #[test]
    fn test_truncate_for_prompt_no_user_messages() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::assistant("Asst"),
        ];

        // No user messages, so target_prompt_index 0 keeps everything
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 2);
    }

    #[test]
    fn test_truncate_for_prompt_skips_interspersed_system_reminders() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"),
            ConversationItem::assistant("Asst 1"),
            ConversationItem::system_reminder("<system-reminder>skill update</system-reminder>"),
            ConversationItem::user("User 2"),
            ConversationItem::assistant("Asst 2"),
            ConversationItem::system_reminder("<system-reminder>plan reminder</system-reminder>"),
            ConversationItem::user("User 3"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 4);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 7);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 8);
    }

    #[test]
    fn test_truncate_for_prompt_skips_auto_continue() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"),
            ConversationItem::assistant("Asst 1"),
            ConversationItem::auto_continue("Continue working on the task"),
            ConversationItem::assistant("Continuing..."),
            ConversationItem::user("User 2"),
            ConversationItem::assistant("Asst 2"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 5);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 7);
    }

    #[test]
    fn test_truncate_for_prompt_skips_auto_recovery() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"),
            ConversationItem::assistant("Asst 1"),
            ConversationItem::auto_recovery("Try the tool again"),
            ConversationItem::assistant("Retrying..."),
            ConversationItem::user("User 2"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 5);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 6);
    }

    #[test]
    fn test_truncate_for_prompt_skips_synthetic_user() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("User 1"),
            ConversationItem::assistant("Asst 1"),
            ConversationItem::system_reminder("Stop repeating"),
            ConversationItem::assistant("Retry guidance"),
            ConversationItem::user("User 2"),
            ConversationItem::assistant("Asst 2"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 5);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 7);
    }

    /// Regression (rewind kept the rewound turn): synthetic-origin turns
    /// (auto-wake task/subagent completion, notification drain, scheduler)
    /// consume a prompt_index slot, so the counting fallback must treat their
    /// marker-less user items as turn starts.
    #[test]
    fn test_truncate_for_prompt_counts_marker_less_synthetic_turn_starts() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("<user_info>preamble</user_info>"),
            ConversationItem::user("P0"),
            ConversationItem::assistant("A0"),
            ConversationItem::task_completed("Background task abc completed"), // turn 1
            ConversationItem::assistant("A1"),
            ConversationItem::user("P2"),
            ConversationItem::assistant("A2"),
        ];

        // Rewind to turn 2 cuts at P2.
        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 6);
        // Rewind to the auto-wake turn itself cuts at the wake item.
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 4);
        // Rewind to turn 0 keeps only the preamble prefix.
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 2);
    }

    /// Mid-turn synthetics must NOT count as turn starts even in a session
    /// that also contains turn-start synthetics.
    #[test]
    fn test_truncate_for_prompt_mid_turn_synthetics_do_not_count() {
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("<user_info>preamble</user_info>"),
            ConversationItem::user("P0"),
            ConversationItem::interjection("also do this"), // mid-turn
            ConversationItem::assistant("A0"),
            ConversationItem::scheduler_fired("loop fired"), // turn 1
            ConversationItem::system_reminder("reminder"),   // mid-turn
            ConversationItem::assistant("A1"),
            ConversationItem::user("P2"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 8);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 5);
    }

    /// Explicit `UserItem::prompt_index` markers are authoritative: they
    /// resync the running index regardless of synthetic reasons, and they
    /// locate the cut in compaction-rebuilt prefixes where counting is
    /// structurally wrong.
    #[test]
    fn test_truncate_for_prompt_prefers_prompt_index_markers() {
        let marked = |text: &str, idx: usize| {
            let mut item = ConversationItem::user(text);
            item.set_prompt_index(idx);
            item
        };
        let marked_wake = |text: &str, idx: usize| {
            let mut item = ConversationItem::task_completed(text);
            item.set_prompt_index(idx);
            item
        };

        // Post-compaction shape: rebuilt preamble + carried last-user-query +
        // summary, then a marker-carrying live turn. Counting would assign
        // the carried query index 0 and never find turn 12; the marker does.
        let conversation = vec![
            ConversationItem::system("SP"),
            ConversationItem::user("<user_info>rebuilt</user_info>"),
            ConversationItem::user("carried last user query"),
            ConversationItem::user_meta("summary"),
            marked_wake("Background task xyz completed", 11),
            ConversationItem::assistant("A11"),
            marked("P12", 12),
            ConversationItem::assistant("A12"),
        ];

        assert_eq!(conversation_truncate_for_prompt(&conversation, 12), 6);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 11), 4);
        // Unnumbered user rows do not open turns once markers exist (mid-turn
        // phantoms omit prompt_index). Rewind to 12 finds no marker ≥ 12.
        let mut mixed = conversation.clone();
        mixed[6] = ConversationItem::user("mid-turn phantom");
        assert_eq!(conversation_truncate_for_prompt(&mixed, 12), mixed.len());
        assert_eq!(conversation_truncate_for_prompt(&mixed, 11), 4);
    }

    /// Mid-turn plain users without `prompt_index` must not shift the cut when
    /// real turns carry markers (bash-mode / permission followup shape).
    #[test]
    fn test_truncate_for_prompt_unmarked_phantoms_ignored_when_markers_present() {
        let marked = |text: &str, idx: usize| {
            let mut item = ConversationItem::user(text);
            item.set_prompt_index(idx);
            item
        };
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("<user_info>preamble</user_info>"),
            marked("P0", 0),
            ConversationItem::assistant("A0"),
            ConversationItem::user("!pwd phantom"),
            ConversationItem::assistant("bash out"),
            marked("P1", 1),
            ConversationItem::assistant("A1"),
            ConversationItem::user("permission followup phantom"),
            marked("P2", 2),
            ConversationItem::assistant("A2"),
        ];
        // Cut at P2 (index 2) keeps through followup phantom, drops P2+.
        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 9);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 6);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 2);
    }

    /// Mixed upgrade: unmarked historic turns before the first marker still
    /// count (contiguous with the first absolute index). Phantoms after the
    /// first marker do not. Whole-buffer marker mode would under-cut here.
    #[test]
    fn test_truncate_for_prompt_mixed_unmarked_prefix_then_markers() {
        let marked = |text: &str, idx: usize| {
            let mut item = ConversationItem::user(text);
            item.set_prompt_index(idx);
            item
        };
        // preamble + unmarked P0,P1 + marked P2,P3 with phantom after markers.
        let conversation = vec![
            ConversationItem::system("System"),
            ConversationItem::user("<user_info>preamble</user_info>"),
            ConversationItem::user("old P0"),
            ConversationItem::assistant("A0"),
            ConversationItem::user("old P1"),
            ConversationItem::assistant("A1"),
            marked("new P2", 2),
            ConversationItem::assistant("A2"),
            ConversationItem::user("!pwd phantom"),
            marked("new P3", 3),
            ConversationItem::assistant("A3"),
        ];
        // Cut at 2 keeps through A1 (drops marked P2+).
        assert_eq!(conversation_truncate_for_prompt(&conversation, 2), 6);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 1), 4);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 0), 2);
        assert_eq!(conversation_truncate_for_prompt(&conversation, 3), 9);
    }

    /// The new field round-trips through JSON and is omitted when `None`
    /// (byte-stable with sessions written before the field existed).
    #[test]
    fn test_user_prompt_index_serde_roundtrip() {
        let plain = ConversationItem::user("hello");
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("prompt_index"),
            "None marker must be omitted, got {json}"
        );

        let mut marked = ConversationItem::user("hello");
        marked.set_prompt_index(7);
        let json = serde_json::to_string(&marked).unwrap();
        assert!(json.contains("\"prompt_index\":7"), "got {json}");
        let back: ConversationItem = serde_json::from_str(&json).unwrap();
        match back {
            ConversationItem::User(u) => assert_eq!(u.prompt_index, Some(7)),
            other => panic!("expected user, got {other:?}"),
        }

        // Old sessions (no field) deserialize as None.
        let legacy: ConversationItem =
            serde_json::from_str(r#"{"type":"user","content":[{"type":"text","text":"hi"}]}"#)
                .unwrap();
        match legacy {
            ConversationItem::User(u) => assert_eq!(u.prompt_index, None),
            other => panic!("expected user, got {other:?}"),
        }
    }

    // ============================================================================
    // transform_conversation_cwd Tests
    // ============================================================================

    #[test]
    fn test_transform_cwd_in_system_message() {
        let mut items = vec![ConversationItem::system(
            "You are working in /old/path/to/project",
        )];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(
            items[0].text_content(),
            "You are working in /new/path/to/project"
        );
    }

    #[test]
    fn test_transform_cwd_in_user_message() {
        let mut items = vec![ConversationItem::user("Please edit /old/path/src/main.rs")];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(items[0].text_content(), "Please edit /new/path/src/main.rs");
    }

    #[test]
    fn test_transform_cwd_in_assistant_message() {
        let mut items = vec![ConversationItem::assistant(
            "I found the file at /old/path/src/lib.rs",
        )];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(
            items[0].text_content(),
            "I found the file at /new/path/src/lib.rs"
        );
    }

    #[test]
    fn test_transform_cwd_in_tool_result() {
        let mut items = vec![ConversationItem::tool_result(
            "call_1",
            "Contents of /old/path/file.txt:\nHello world",
        )];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(
            items[0].text_content(),
            "Contents of /new/path/file.txt:\nHello world"
        );
    }

    #[test]
    fn test_transform_cwd_multiple_occurrences() {
        let mut items = vec![ConversationItem::user(
            "/old/path/a.txt and /old/path/b.txt and /old/path/c.txt",
        )];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(
            items[0].text_content(),
            "/new/path/a.txt and /new/path/b.txt and /new/path/c.txt"
        );
    }

    #[test]
    fn test_transform_cwd_no_match() {
        let mut items = vec![ConversationItem::user("No paths here")];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        assert_eq!(items[0].text_content(), "No paths here");
    }

    #[test]
    fn test_transform_cwd_with_image_content() {
        let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "Look at /old/path/image.png".into(),
            },
            ContentPart::Image {
                url: "https://example.com/img.png".into(),
            },
        ])];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        if let ConversationItem::User(u) = &items[0] {
            if let ContentPart::Text { text } = &u.content[0] {
                assert_eq!(text.as_ref(), "Look at /new/path/image.png");
            }
            // Image URL should not be transformed
            if let ContentPart::Image { url } = &u.content[1] {
                assert_eq!(url.as_ref(), "https://example.com/img.png");
            }
        }
    }

    // ============================================================================
    // transform_conversation_cwd Tool Call & Edge Case Tests
    // ============================================================================

    #[test]
    fn test_transform_cwd_transforms_tool_call_arguments() {
        // Tool call arguments containing paths are transformed alongside text content.
        // This ensures the model sees consistent paths on the next turn.
        let worktree = "/home/user/.echo-agent/worktrees/project/ab-uuid-a";
        let root = "/home/user/project";

        let mut items = vec![ConversationItem::Assistant(AssistantItem {
            content: format!("I'll read the file at {worktree}/src/main.rs").into(),
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: format!(r#"{{"target_file":"{worktree}/src/main.rs"}}"#).into(),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "search_replace".to_string(),
                    arguments: format!(
                        r#"{{"file_path":"{worktree}/src/lib.rs","old_string":"foo","new_string":"bar"}}"#
                    ).into(),
                },
                ToolCall {
                    id: "call_3".into(),
                    name: "run_terminal_cmd".to_string(),
                    arguments: format!(
                        r#"{{"command":"cargo test --manifest-path {worktree}/Cargo.toml"}}"#
                    ).into(),
                },
            ],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })];

        transform_conversation_cwd(&mut items, worktree, root);

        // Content is transformed
        assert_eq!(
            items[0].text_content(),
            format!("I'll read the file at {root}/src/main.rs")
        );

        // Tool call arguments are also transformed
        if let ConversationItem::Assistant(a) = &items[0] {
            assert!(
                a.tool_calls[0].arguments.contains(root),
                "read_file arguments should contain root path"
            );
            assert!(
                !a.tool_calls[0].arguments.contains(worktree),
                "read_file arguments should not contain worktree path"
            );
            assert!(
                a.tool_calls[1].arguments.contains(root),
                "search_replace arguments should contain root path"
            );
            assert!(
                !a.tool_calls[1].arguments.contains(worktree),
                "search_replace arguments should not contain worktree path"
            );
            assert!(
                a.tool_calls[2].arguments.contains(root),
                "run_terminal_cmd arguments should contain root path"
            );
            assert!(
                !a.tool_calls[2].arguments.contains(worktree),
                "run_terminal_cmd arguments should not contain worktree path"
            );
        } else {
            panic!("Expected Assistant item");
        }
    }

    #[test]
    fn test_transform_cwd_worktree_to_root_syncback() {
        // End-to-end sync-back scenario: worktree paths -> root paths
        // This simulates what happens when a forked session's worktree
        // contents are synced back to the original root path.
        let worktree = "/home/user/.echo-agent/worktrees/myproject/fork-a";
        let root = "/home/user/myproject";

        let mut items = vec![
            // System prompt with worktree cwd
            ConversationItem::system(format!(
                "You are an AI assistant. The user's workspace is at {worktree}."
            )),
            // User message (typically doesn't have worktree paths, but could)
            ConversationItem::user("Fix the bug in main.rs"),
            // Assistant saying what it found
            ConversationItem::assistant(format!(
                "I found the issue in {worktree}/src/main.rs at line 42."
            )),
            // Tool result from read_file
            ConversationItem::tool_result(
                "call_1",
                format!("Contents of {worktree}/src/main.rs:\nfn main() {{}}\n"),
            ),
            // Assistant with tool calls for the fix
            ConversationItem::Assistant(AssistantItem {
                content: format!("I'll fix the bug in {worktree}/src/main.rs").into(),
                tool_calls: vec![ToolCall {
                    id: "call_2".into(),
                    name: "search_replace".to_string(),
                    arguments: format!(
                        r#"{{"file_path":"{worktree}/src/main.rs","old_string":"fn main() {{}}","new_string":"fn main() {{\n    println!(\"Hello\");\n}}"}}"#
                    ).into(),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
        ];

        transform_conversation_cwd(&mut items, worktree, root);

        // All text content should be transformed
        assert!(items[0].text_content().contains(root));
        assert!(!items[0].text_content().contains(worktree));

        assert!(items[2].text_content().contains(root));
        assert!(!items[2].text_content().contains(worktree));

        assert!(items[3].text_content().contains(root));
        assert!(!items[3].text_content().contains(worktree));

        assert!(items[4].text_content().contains(root));
        assert!(!items[4].text_content().contains(worktree));

        // Tool call arguments are also transformed (worktree → root)
        if let ConversationItem::Assistant(a) = &items[4] {
            assert!(
                a.tool_calls[0].arguments.contains(root),
                "tool_call arguments should contain root path after sync-back"
            );
            assert!(
                !a.tool_calls[0].arguments.contains(worktree),
                "tool_call arguments should not contain worktree path after sync-back"
            );
        }
    }

    #[test]
    fn test_transform_cwd_forward_fork_root_to_worktree() {
        // Forward direction: root → worktree (forking)
        // Tool call arguments are transformed so the fork session's history
        // has consistent worktree paths everywhere.
        let root = "/home/user/myproject";
        let worktree = "/home/user/.echo-agent/worktrees/myproject/fork-a";

        let mut items = vec![
            ConversationItem::system(format!("Working in {root}.")),
            ConversationItem::Assistant(AssistantItem {
                content: format!("I previously edited {root}/src/main.rs").into(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: format!(r#"{{"target_file":"{root}/src/main.rs"}}"#).into(),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
        ];

        transform_conversation_cwd(&mut items, root, worktree);

        // Text content transformed to worktree
        assert!(items[0].text_content().contains(worktree));

        // Tool calls also transformed to worktree
        if let ConversationItem::Assistant(a) = &items[1] {
            assert!(
                a.tool_calls[0].arguments.contains(worktree),
                "tool_call arguments should contain worktree path after forward fork"
            );
            assert!(
                !a.tool_calls[0].arguments.contains(root),
                "tool_call arguments should not contain root path after forward fork"
            );
        }
    }

    #[test]
    fn test_transform_cwd_no_false_positives_on_partial_match() {
        // Ensure we don't accidentally match a prefix that's a substring
        let mut items = vec![ConversationItem::user(
            "/home/user/myproject-extra/src/main.rs and /home/user/myproject/src/lib.rs",
        )];

        transform_conversation_cwd(&mut items, "/home/user/myproject", "/new/path");

        // Both paths get transformed because str::replace does substring matching.
        // "/home/user/myproject-extra" contains "/home/user/myproject" as a prefix,
        // so it becomes "/new/path-extra" — this is Open Question 3 (false positives).
        assert_eq!(
            items[0].text_content(),
            "/new/path-extra/src/main.rs and /new/path/src/lib.rs"
        );
    }

    #[test]
    fn test_transform_cwd_empty_source_noop() {
        // Edge case: source and target are the same (no transform needed)
        let mut items = vec![ConversationItem::user("Hello at /some/path/file.rs")];

        transform_conversation_cwd(&mut items, "/some/path", "/some/path");

        // Content should be unchanged
        assert_eq!(items[0].text_content(), "Hello at /some/path/file.rs");
    }

    #[test]
    fn test_transform_cwd_mixed_conversation_full() {
        // Full conversation with all item types and multiple path occurrences
        let src = "/worktree/abc";
        let dst = "/root/project";

        let mut items = vec![
            ConversationItem::system(format!("Workspace: {src}")),
            ConversationItem::user_with_parts(vec![
                ContentPart::Text {
                    text: format!("Edit {src}/a.rs").into(),
                },
                ContentPart::Text {
                    text: format!("And {src}/b.rs").into(),
                },
            ]),
            ConversationItem::assistant(format!("Done with {src}/a.rs and {src}/b.rs")),
            ConversationItem::tool_result("call_1", format!("File {src}/a.rs saved")),
        ];

        transform_conversation_cwd(&mut items, src, dst);

        // System
        assert_eq!(items[0].text_content(), format!("Workspace: {dst}"));

        // User - both text parts
        if let ConversationItem::User(u) = &items[1] {
            if let ContentPart::Text { text } = &u.content[0] {
                assert_eq!(text.as_ref(), format!("Edit {dst}/a.rs").as_str());
            }
            if let ContentPart::Text { text } = &u.content[1] {
                assert_eq!(text.as_ref(), format!("And {dst}/b.rs").as_str());
            }
        }

        // Assistant text
        assert_eq!(
            items[2].text_content(),
            format!("Done with {dst}/a.rs and {dst}/b.rs")
        );

        // Tool result
        assert_eq!(items[3].text_content(), format!("File {dst}/a.rs saved"));
    }

    #[test]
    fn test_transform_cwd_tool_calls_with_no_paths() {
        // Tool calls that don't contain any paths should be unaffected
        let mut items = vec![ConversationItem::Assistant(AssistantItem {
            content: "Running a command".into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "run_terminal_cmd".to_string(),
                arguments: r#"{"command":"echo hello"}"#.into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })];

        transform_conversation_cwd(&mut items, "/old/path", "/new/path");

        if let ConversationItem::Assistant(a) = &items[0] {
            assert_eq!(
                a.tool_calls[0].arguments.as_ref(),
                r#"{"command":"echo hello"}"#
            );
        }
    }

    #[test]
    fn test_transform_cwd_assistant_only_tool_calls_no_content() {
        // Assistant message with empty content but tool calls containing paths
        let worktree = "/home/user/.echo-agent/worktrees/proj/fork-a";
        let root = "/home/user/proj";

        let mut items = vec![ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: format!(r#"{{"target_file":"{worktree}/src/main.rs"}}"#).into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "grep".to_string(),
                arguments: format!(r#"{{"pattern":"TODO","path":"{worktree}/src"}}"#).into(),
            },
        ])];

        transform_conversation_cwd(&mut items, worktree, root);

        // Content is empty, so no transform there
        assert_eq!(items[0].text_content(), "");

        // Tool call arguments are transformed
        if let ConversationItem::Assistant(a) = &items[0] {
            assert!(
                a.tool_calls[0].arguments.contains(root),
                "read_file arguments should contain root path"
            );
            assert!(
                !a.tool_calls[0].arguments.contains(worktree),
                "read_file arguments should not contain worktree path"
            );
            assert!(
                a.tool_calls[1].arguments.contains(root),
                "grep arguments should contain root path"
            );
            assert!(
                !a.tool_calls[1].arguments.contains(worktree),
                "grep arguments should not contain worktree path"
            );
        }
    }

    // ============================================================================
    // ConversationResponse Tests
    // ============================================================================

    #[test]
    fn test_conversation_response_is_empty() {
        // Empty assistant message
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("")],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(response.is_empty());

        // Assistant with content
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("Hello")],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 1,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(!response.is_empty());

        // Assistant with only tool calls
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "1".into(),
                name: "test".to_string(),
                arguments: "{}".into(),
            }])],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(!response.is_empty());
    }

    #[test]
    fn test_is_empty_with_reasoning_but_no_content() {
        // The model returned reasoning tokens but no visible content.
        // is_empty() should return true so the retry logic resamples.
        let response = ConversationResponse {
            items: vec![ConversationItem::Assistant(AssistantItem {
                content: String::new().into(),
                tool_calls: vec![],
                model_id: Some("test-model".to_string()),
                model_fingerprint: None,
                reasoning_effort: None,
            })],
            stop_reason: Some(StopReason::Stop),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(
            response.is_empty(),
            "reasoning-only response should be considered empty"
        );

        // Reasoning with content should NOT be empty
        let response = ConversationResponse {
            items: vec![ConversationItem::Assistant(AssistantItem {
                content: "Here is my answer.".into(),
                tool_calls: vec![],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            })],
            stop_reason: Some(StopReason::Stop),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 1,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(
            !response.is_empty(),
            "reasoning with content should not be empty"
        );

        // Reasoning with tool calls should NOT be empty
        let response = ConversationResponse {
            items: vec![ConversationItem::Assistant(AssistantItem {
                content: String::new().into(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".to_string(),
                    arguments: "{}".into(),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            })],
            stop_reason: Some(StopReason::ToolCalls),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(
            !response.is_empty(),
            "reasoning with tool calls should not be empty"
        );
    }

    #[test]
    fn test_conversation_response_tool_calls() {
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant_tool_calls(vec![
                ToolCall {
                    id: "1".into(),
                    name: "read_file".to_string(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "2".into(),
                    name: "bash".to_string(),
                    arguments: "{}".into(),
                },
            ])],
            stop_reason: Some(StopReason::ToolCalls),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };

        let calls = response.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[1].name, "bash");
    }

    #[test]
    fn test_fallback_text_after_empty_response_retry() {
        // Scenario: empty-response retry — text present but no
        // AgentMessageChunk events were streamed.
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("All features implemented.")],
            stop_reason: Some(StopReason::Stop),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert_eq!(
            response.fallback_text().as_deref(),
            Some("All features implemented.")
        );
    }

    #[test]
    fn test_fallback_text_none_when_chunks_streamed() {
        // Normal streaming: text was already delivered via AgentMessageChunk
        // events, so no fallback is needed.
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("Hello")],
            stop_reason: Some(StopReason::Stop),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 42,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(response.fallback_text().is_none());
    }

    #[test]
    fn test_fallback_text_none_for_empty_response() {
        // Truly empty response (no content, no chunks): no fallback.
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("")],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(response.fallback_text().is_none());
    }

    #[test]
    fn test_fallback_text_fires_for_reasoning_only_stream() {
        // Reasoning-only scenario: the model produced only thought chunks
        // (which increment chunk_index but NOT message_chunks_emitted).
        // The final text was surfaced at completion time, so
        // message_chunks_emitted is 0 even though the model did produce
        // content.  The fallback MUST fire in this case.
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant("Summary after reasoning.")],
            stop_reason: Some(StopReason::Stop),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0, // only reasoning chunks were streamed
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert_eq!(
            response.fallback_text().as_deref(),
            Some("Summary after reasoning.")
        );
    }

    #[test]
    fn test_fallback_text_none_for_tool_call_only_response() {
        // Tool-call-only response: no text content, no fallback needed.
        let response = ConversationResponse {
            items: vec![ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            }])],
            stop_reason: Some(StopReason::ToolCalls),
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 0,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };
        assert!(response.fallback_text().is_none());
    }

    // ============================================================================
    // StopReason Conversion Tests
    // ============================================================================

    #[test]
    fn test_stop_reason_from_finish_reason() {
        assert_eq!(StopReason::from(FinishReason::Stop), StopReason::Stop);
        assert_eq!(StopReason::from(FinishReason::Length), StopReason::Length);
        assert_eq!(
            StopReason::from(FinishReason::ToolCalls),
            StopReason::ToolCalls
        );
        assert_eq!(
            StopReason::from(FinishReason::FunctionCall),
            StopReason::ToolCalls
        );
        assert_eq!(
            StopReason::from(FinishReason::ContentFilter),
            StopReason::ContentFilter
        );
    }

    // ============================================================================
    // Builder Pattern Tests
    // ============================================================================

    #[test]
    fn test_conversation_request_builder() {
        let req = ConversationRequest::new()
            .with_model("echo-agent-3")
            .with_temperature(0.5)
            .with_max_output_tokens(1000)
            .with_conv_id("conv-123")
            .with_req_id("req-456")
            .with_tool_choice(ConversationToolChoice::Auto);

        assert_eq!(req.model, Some("echo-agent-3".to_string()));
        assert_eq!(req.temperature, Some(0.5));
        assert_eq!(req.max_output_tokens, Some(1000));
        assert_eq!(req.x_echo_agent_conv_id, Some("conv-123".to_string()));
        assert_eq!(req.x_echo_agent_req_id, Some("req-456".to_string()));
        assert_matches!(req.tool_choice, Some(ConversationToolChoice::Auto));
    }

    #[test]
    fn test_conversation_request_push() {
        let mut req = ConversationRequest::new();
        assert!(req.items.is_empty());

        req.push(ConversationItem::system("System"));
        req.push(ConversationItem::user("User"));

        assert_eq!(req.items.len(), 2);
    }

    #[test]
    fn test_assistant_item_with_model_id() {
        let item = AssistantItem {
            content: "Hello".into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }
        .with_model_id("echo-agent-3");

        assert_eq!(item.model_id, Some("echo-agent-3".to_string()));
    }

    #[test]
    fn test_conversation_item_with_model_id() {
        let item = ConversationItem::assistant("Hello").with_model_id("echo-agent-3");

        let ConversationItem::Assistant(a) = item else {
            panic!("Expected Assistant");
        };
        assert_eq!(a.model_id, Some("echo-agent-3".to_string()));

        // Non-assistant should be unchanged
        let user = ConversationItem::user("Hi").with_model_id("echo-agent-3");
        assert_matches!(user, ConversationItem::User(_));
    }

    // ============================================================================
    // Serialization Tests
    // ============================================================================

    #[test]
    fn test_conversation_item_serialization() {
        let items = vec![
            ConversationItem::system("System prompt"),
            ConversationItem::user("User message"),
            ConversationItem::assistant("Assistant response"),
            ConversationItem::tool_result("call_1", "Tool output"),
        ];

        for item in &items {
            let json = serde_json::to_string(item).expect("Should serialize");
            let back: ConversationItem = serde_json::from_str(&json).expect("Should deserialize");
            assert_eq!(item.text_content(), back.text_content());
        }
    }

    #[test]
    fn test_tool_call_serialization() {
        let tool_call = ToolCall {
            id: "call_123".into(),
            name: "bash".to_string(),
            arguments: r#"{"command": "ls"}"#.into(),
        };

        let json = serde_json::to_string(&tool_call).expect("Should serialize");
        let back: ToolCall = serde_json::from_str(&json).expect("Should deserialize");

        assert_eq!(back.id.as_ref(), "call_123");
        assert_eq!(back.name, "bash");
        assert_eq!(back.arguments.as_ref(), r#"{"command": "ls"}"#);
    }

    #[test]
    fn test_reasoning_content_serialization() {
        let reasoning = ReasoningContent {
            text: Some("Thinking...".into()),
            encrypted: Some("enc_data".into()),
            id: None,
        };

        let json = serde_json::to_string(&reasoning).expect("Should serialize");
        let back: ReasoningContent = serde_json::from_str(&json).expect("Should deserialize");

        assert_eq!(back.text.as_deref(), Some("Thinking..."));
        assert_eq!(back.encrypted.as_deref(), Some("enc_data"));
    }

    #[test]
    fn test_repair_no_tool_calls() {
        let mut conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("hello"),
            ConversationItem::assistant("hi"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            0
        );
        assert_eq!(conv.len(), 3);
    }

    #[test]
    fn test_repair_all_answered() {
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::tool_result("c1", "ok"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            0
        );
    }

    #[test]
    fn test_repair_single_dangling() {
        let mut conv = vec![
            ConversationItem::user("hello"),
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 3);
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert!(tr.content.contains("cancelled"));
            assert!(tr.content.contains("run_terminal_cmd"));
        });
    }

    #[test]
    fn test_has_dangling_tool_calls() {
        // No tool calls → not dangling.
        assert!(!has_dangling_tool_calls(&[
            ConversationItem::user("hello"),
            ConversationItem::assistant("hi"),
        ]));
        // Fully answered tool call → not dangling.
        assert!(!has_dangling_tool_calls(&[
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::tool_result("c1", "ok"),
        ]));
        // Unanswered tool call (mid-tool / parked-on-permission case) → dangling.
        assert!(has_dangling_tool_calls(&[
            ConversationItem::user("hello"),
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
        ]));
        // Partially answered parallel calls → dangling.
        assert!(has_dangling_tool_calls(&[
            assistant_with_calls(&[("c1", "a"), ("c2", "b")]),
            ConversationItem::tool_result("c1", "ok"),
        ]));
    }

    #[test]
    fn test_repair_single_dangling_harness_halted() {
        let mut conv = vec![
            ConversationItem::user("hello"),
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
        ];
        assert_eq!(
            repair_dangling_tool_calls(
                &mut conv,
                DanglingToolCallReason::HarnessHalted {
                    class: "policy_guard",
                },
            ),
            1
        );
        assert_eq!(conv.len(), 3);
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert_eq!(
                tr.content.as_ref(),
                "Tool execution was halted by the harness (policy_guard); \
                 the tool `run_terminal_cmd` was not executed.",
            );
            assert!(!tr.content.contains("user"));
        });
    }

    #[test]
    fn test_repair_multiple_dangling_with_harness_halted_preserves_order() {
        // Two parallel dangling tool calls, both rendered with the
        // harness-halted wording, must be appended in original call
        // order with each carrying its own tool name.
        let mut conv = vec![assistant_with_calls(&[
            ("read_call_1", "read_file"),
            ("grep_call_2", "grep"),
        ])];
        assert_eq!(
            repair_dangling_tool_calls(
                &mut conv,
                DanglingToolCallReason::HarnessHalted {
                    class: "policy_guard",
                },
            ),
            2
        );
        assert_eq!(conv.len(), 3);
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "read_call_1");
            assert!(tr.content.contains("`read_file`"));
            assert!(tr.content.contains("policy_guard"));
        });
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "grep_call_2");
            assert!(tr.content.contains("`grep`"));
        });
    }

    #[test]
    fn test_repair_multiple_dangling() {
        let mut conv = vec![assistant_with_calls(&[("c1", "read_file"), ("c2", "grep")])];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            2
        );
        assert_eq!(conv.len(), 3);
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
        });
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c2");
        });
    }

    #[test]
    fn test_repair_partial() {
        // call_1 answered, call_2 not
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file"), ("c2", "grep")]),
            ConversationItem::tool_result("c1", "file contents"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 3);
        // Existing result stays at index 1, synthetic inserted after it
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert_eq!(tr.content.as_ref(), "file contents");
        });
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c2");
            assert!(tr.content.contains("cancelled"));
        });
    }

    #[test]
    fn test_repair_idempotent() {
        let mut conv = vec![
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
            ConversationItem::user("hi"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 3);
        // Synthetic result inserted right after the assistant, before the user message
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
        });
        assert_matches!(&conv[2], ConversationItem::User(_));
        // Second call: nothing to repair
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            0
        );
        assert_eq!(conv.len(), 3);
    }

    #[test]
    fn test_repair_first_turn_resolved_second_dangling() {
        // First turn fully resolved, second turn dangling
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::tool_result("c1", "ok"),
            ConversationItem::assistant("done"),
            ConversationItem::user("second"),
            assistant_with_calls(&[("c2", "run_terminal_cmd")]),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 6);
        assert_matches!(&conv[5], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c2");
        });
    }

    #[test]
    fn test_repair_dangling_before_text_only_assistant() {
        // First assistant has dangling tool call, second is text-only.
        // The full scan must still repair the first assistant's dangling call.
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::assistant("text-only response"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 3);
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert!(tr.content.contains("cancelled"));
        });
        assert_matches!(&conv[2], ConversationItem::Assistant(a) => {
            assert!(a.tool_calls.is_empty());
        });
    }

    #[test]
    fn test_repair_inserts_after_last_tool_result() {
        // Assistant made 3 calls, first two answered, third dangling.
        // There's a user message after the tool results. The synthetic result
        // should be inserted after the last tool result, before the user message.
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file"), ("c2", "grep"), ("c3", "bash")]),
            ConversationItem::tool_result("c1", "file contents"),
            ConversationItem::tool_result("c2", "grep results"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            1
        );
        assert_eq!(conv.len(), 4);
        // c1 result at index 1, c2 result at index 2, synthetic c3 at index 3
        assert_matches!(&conv[3], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c3");
            assert!(tr.content.contains("cancelled"));
        });
    }

    #[test]
    fn test_repair_inserts_before_trailing_user_message() {
        // Assistant made calls, no answers, then user sent a message.
        // Synthetic results should go right after the assistant, before the user message.
        let mut conv = vec![
            ConversationItem::user("do stuff"),
            assistant_with_calls(&[("c1", "read_file"), ("c2", "grep")]),
            ConversationItem::user("never mind"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            2
        );
        assert_eq!(conv.len(), 5);
        // Original: [user, assistant, user]
        // After:    [user, assistant, tool(c1), tool(c2), user]
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
        });
        assert_matches!(&conv[3], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c2");
        });
        assert_matches!(&conv[4], ConversationItem::User(_));
    }

    #[test]
    fn test_repair_multiple_assistants_dangling_throughout() {
        // Simulates an old session where the user interrupted multiple tool calls
        // across the conversation. All dangling calls must be repaired, not just
        // the last one.
        let mut conv = vec![
            ConversationItem::user("hello"),
            // Turn 1: assistant makes a call, user interrupts
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
            ConversationItem::user("no, the repo is already cloned"),
            // Turn 2: assistant works normally
            assistant_with_calls(&[("c2", "read_file")]),
            ConversationItem::tool_result("c2", "file contents"),
            ConversationItem::assistant("here's what I found"),
            // Turn 3: assistant makes a call, user interrupts again
            ConversationItem::user("now do something else"),
            assistant_with_calls(&[("c3", "run_terminal_cmd")]),
            ConversationItem::user("actually never mind"),
            // Turn 4: assistant makes a call, user interrupts yet again
            assistant_with_calls(&[("c4", "grep")]),
        ];
        // c1, c3, c4 are dangling; c2 is answered → 3 repairs
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            3
        );
        // 10 original + 3 synthetic = 13
        assert_eq!(conv.len(), 13);
        // After repair the conversation should be:
        //  0: user("hello")
        //  1: assistant([c1])
        //  2: tool_result(c1)  ← synthetic
        //  3: user("no, the repo is already cloned")
        //  4: assistant([c2])
        //  5: tool_result(c2)
        //  6: assistant("here's what I found")
        //  7: user("now do something else")
        //  8: assistant([c3])
        //  9: tool_result(c3)  ← synthetic
        // 10: user("actually never mind")
        // 11: assistant([c4])
        // 12: tool_result(c4)  ← synthetic
        assert_matches!(&conv[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert!(tr.content.contains("cancelled"));
        });
        assert_matches!(&conv[9], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c3");
            assert!(tr.content.contains("cancelled"));
        });
        assert_matches!(&conv[12], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c4");
            assert!(tr.content.contains("cancelled"));
        });
        // Idempotent: second run should find nothing
        assert_eq!(
            repair_dangling_tool_calls(&mut conv, DanglingToolCallReason::UserCancelled),
            0
        );
    }

    // ====================================================================
    // dedup_duplicate_tool_results tests
    // ====================================================================

    #[test]
    fn test_dedup_no_duplicates() {
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file"), ("c2", "grep")]),
            ConversationItem::tool_result("c1", "ok"),
            ConversationItem::tool_result("c2", "found it"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 0);
        assert_eq!(conv.len(), 3);
    }

    #[test]
    fn test_dedup_single_duplicate_keeps_last() {
        // This is the exact bug scenario: cancelled result followed by real result.
        let mut conv = vec![
            assistant_with_calls(&[("c1", "run_terminal_cmd")]),
            ConversationItem::tool_result(
                "c1",
                "Tool execution was cancelled by the user (tool `run_terminal_cmd` was not executed).",
            ),
            ConversationItem::tool_result("c1", "exit: 0\nreal output here"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 1);
        assert_eq!(conv.len(), 2); // assistant + 1 tool_result
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert!(tr.content.contains("real output here"));
        });
    }

    #[test]
    fn test_dedup_multiple_calls_one_has_dup() {
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file"), ("c2", "grep")]),
            ConversationItem::tool_result("c1", "cancelled"),
            ConversationItem::tool_result("c2", "found it"),
            ConversationItem::tool_result("c1", "real content"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 1);
        assert_eq!(conv.len(), 3); // assistant + 2 tool_results
        // c1 should be the real content (last occurrence)
        let c1_results: Vec<_> = conv
            .iter()
            .filter_map(|item| {
                if let ConversationItem::ToolResult(tr) = item
                    && tr.tool_call_id == "c1"
                {
                    return Some(tr.content.as_ref());
                }
                None
            })
            .collect();
        assert_eq!(c1_results, vec!["real content"]);
    }

    #[test]
    fn test_dedup_no_tool_calls() {
        let mut conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("hello"),
            ConversationItem::assistant("hi"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 0);
        assert_eq!(conv.len(), 3);
    }

    #[test]
    fn test_dedup_empty_conversation() {
        let mut conv = vec![];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 0);
    }

    #[test]
    fn test_dedup_multiple_assistant_messages() {
        // Two assistant messages, each with a duplicate.
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::tool_result("c1", "old"),
            ConversationItem::tool_result("c1", "new"),
            ConversationItem::user("ok"),
            assistant_with_calls(&[("c2", "grep")]),
            ConversationItem::tool_result("c2", "stale"),
            ConversationItem::tool_result("c2", "fresh"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 2);
        assert_eq!(conv.len(), 5); // 2 assistants + 2 tool_results + 1 user
        assert_matches!(&conv[1], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c1");
            assert_eq!(tr.content.as_ref(), "new");
        });
        assert_matches!(&conv[4], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.tool_call_id, "c2");
            assert_eq!(tr.content.as_ref(), "fresh");
        });
    }

    #[test]
    fn test_dedup_idempotent() {
        let mut conv = vec![
            assistant_with_calls(&[("c1", "read_file")]),
            ConversationItem::tool_result("c1", "cancelled"),
            ConversationItem::tool_result("c1", "real"),
        ];
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 1);
        assert_eq!(conv.len(), 2);
        // Second run should be a no-op.
        assert_eq!(dedup_duplicate_tool_results(&mut conv), 0);
        assert_eq!(conv.len(), 2);
    }

    // ========== strip_images tests ==========

    #[test]
    fn test_strip_images_removes_user_images() {
        let mut req = ConversationRequest::default();
        let mut user = ConversationItem::user("describe this");
        user.add_image("data:image/png;base64,abc123".to_string());
        req.items.push(user);

        let stripped = req.strip_images();
        assert_eq!(stripped.len(), 1);

        // Verify image was replaced with placeholder text
        if let ConversationItem::User(user) = &req.items[0] {
            assert_eq!(user.content.len(), 2); // original text + replaced image
            assert_matches!(&user.content[1], ContentPart::Text { text } => {
                assert!(text.contains("image removed"));
            });
        } else {
            panic!("Expected User item");
        }
    }

    #[test]
    fn test_strip_images_returns_zero_when_no_images() {
        let mut req = ConversationRequest::default();
        req.items.push(ConversationItem::user("just text"));
        req.items.push(ConversationItem::system("system prompt"));
        req.items.push(ConversationItem::assistant("response"));

        let stripped = req.strip_images();
        assert_eq!(stripped.len(), 0);
    }

    #[test]
    fn test_strip_images_leaves_text_unchanged() {
        let mut req = ConversationRequest::default();
        req.items.push(ConversationItem::user("hello world"));

        req.strip_images();

        if let ConversationItem::User(user) = &req.items[0] {
            assert_eq!(user.content.len(), 1);
            assert_matches!(&user.content[0], ContentPart::Text { text } => {
                assert_eq!(text.as_ref(), "hello world");
            });
        } else {
            panic!("Expected User item");
        }
    }

    #[test]
    fn test_strip_images_ignores_system_assistant_tool_items() {
        let mut req = ConversationRequest::default();
        req.items.push(ConversationItem::system("system prompt"));
        req.items.push(ConversationItem::assistant("response"));
        req.items
            .push(ConversationItem::tool_result("call-1", "result text"));

        let stripped = req.strip_images();
        assert_eq!(stripped.len(), 0);

        // Verify nothing was modified
        assert_matches!(&req.items[0], ConversationItem::System(s) => {
            assert_eq!(s.content.as_ref(), "system prompt");
        });
        assert_matches!(&req.items[1], ConversationItem::Assistant(a) => {
            assert_eq!(a.content.as_ref(), "response");
        });
        assert_matches!(&req.items[2], ConversationItem::ToolResult(tr) => {
            assert_eq!(tr.content.as_ref(), "result text");
        });
    }

    #[test]
    fn test_strip_images_mixed_content_only_replaces_images() {
        let mut req = ConversationRequest::default();
        let mut user = ConversationItem::user("look at these");
        user.add_image("data:image/png;base64,img1".to_string());
        req.items.push(user);

        req.strip_images();

        if let ConversationItem::User(user) = &req.items[0] {
            assert_eq!(user.content.len(), 2);
            // Text part preserved
            assert_matches!(&user.content[0], ContentPart::Text { text } => {
                assert_eq!(text.as_ref(), "look at these");
            });
            // Image part replaced
            assert_matches!(&user.content[1], ContentPart::Text { text } => {
                assert!(text.contains("image removed"));
            });
        } else {
            panic!("Expected User item");
        }
    }

    #[test]
    fn test_strip_images_multiple_user_items_with_images() {
        let mut req = ConversationRequest::default();

        let mut user1 = ConversationItem::user("first");
        user1.add_image("data:image/png;base64,aaa".to_string());
        user1.add_image("data:image/png;base64,bbb".to_string());
        req.items.push(user1);

        req.items.push(ConversationItem::assistant("ok"));

        let mut user2 = ConversationItem::user("second");
        user2.add_image("data:image/png;base64,ccc".to_string());
        req.items.push(user2);

        let stripped = req.strip_images();
        assert_eq!(stripped.len(), 3);
    }

    #[test]
    fn test_strip_images_clears_tool_result_images() {
        let mut req = ConversationRequest::default();
        req.items.push(ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![
                ContentPart::Image {
                    url: "data:image/png;base64,aaa".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,bbb".into(),
                },
            ],
        ));

        let stripped = req.strip_images();
        assert_eq!(stripped.len(), 2);

        // Images should be cleared
        if let ConversationItem::ToolResult(t) = &req.items[0] {
            assert!(t.images.is_empty(), "images should be cleared after strip");
            assert_eq!(
                t.content.as_ref(),
                "Read image file: photo.png",
                "text content preserved"
            );
        } else {
            panic!("Expected ToolResult");
        }
    }

    /// The URL-scoped strip: listed URLs are stripped from both part kinds
    /// (User images → placeholder, ToolResult images removed), unlisted
    /// images survive, and the count reflects parts stripped.
    #[test]
    fn test_strip_images_by_url_strips_only_listed_urls() {
        let listed: Arc<str> = "data:image/png;base64,aaa".into();
        let mut user = ConversationItem::user("look");
        user.add_image(listed.to_string());
        user.add_image("data:image/png;base64,unlisted".to_string());
        let mut items = vec![
            user,
            ConversationItem::tool_result_with_images(
                "call_1",
                "read photo.png",
                vec![
                    ContentPart::Image {
                        url: listed.clone(),
                    },
                    ContentPart::Image {
                        url: "data:image/png;base64,unlisted".into(),
                    },
                ],
            ),
        ];

        assert_eq!(strip_images_by_url(&mut items, &[listed]), 2);

        // The whole safety case for persisting via `replace_history`: an
        // in-place strip never changes item count or ordering.
        assert_eq!(items.len(), 2, "strip must never add or remove items");

        let ConversationItem::User(u) = &items[0] else {
            panic!("expected User");
        };
        assert!(
            u.content.iter().any(|p| matches!(
                p,
                ContentPart::Text { text } if text.as_ref() == IMAGE_STRIP_PLACEHOLDER
            )),
            "listed user image must be replaced by the strip placeholder: {:?}",
            u.content
        );
        let user_image_urls: Vec<_> = u
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Image { url } => Some(url.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_image_urls,
            ["data:image/png;base64,unlisted"],
            "listed user image replaced, unlisted survives"
        );
        let ConversationItem::ToolResult(t) = &items[1] else {
            panic!("expected ToolResult");
        };
        assert!(
            matches!(&t.images[..], [ContentPart::Image { url }] if url.contains("unlisted")),
            "listed tool image removed, unlisted survives: {:?}",
            t.images
        );
    }

    #[test]
    fn test_tool_result_with_images_serde_round_trip() {
        let item = ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        );
        let json = serde_json::to_string(&item).expect("serialize");
        let back: ConversationItem = serde_json::from_str(&json).expect("deserialize");

        if let ConversationItem::ToolResult(t) = &back {
            assert_eq!(t.images.len(), 1);
            assert!(matches!(&t.images[0], ContentPart::Image { url } if url.contains("iVBOR")));
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn test_tool_result_without_images_serde_omits_field() {
        let item = ConversationItem::tool_result("call_1", "just text");
        let json = serde_json::to_string(&item).expect("serialize");
        // "images" key should not appear in JSON when empty
        assert!(
            !json.contains("images"),
            "empty images should be omitted: {json}"
        );

        let back: ConversationItem = serde_json::from_str(&json).expect("deserialize");
        if let ConversationItem::ToolResult(t) = &back {
            assert!(t.images.is_empty());
        } else {
            panic!("Expected ToolResult");
        }
    }

    // ── SyntheticReason tests ─────────────────────────────────────────────────

    /// Real user messages must have `synthetic_reason = None`.
    #[test]
    fn user_message_has_no_synthetic_reason() {
        let item = ConversationItem::user("hello");
        if let ConversationItem::User(u) = item {
            assert!(
                u.synthetic_reason.is_none(),
                "real user messages must not have a synthetic_reason"
            );
        } else {
            panic!("expected User variant");
        }
    }

    /// Historical `doom_loop_warning` tags deserialize as Unknown after removal.
    #[test]
    fn historical_doom_loop_warning_deserializes_as_unknown() {
        let json = serde_json::json!({
            "type": "user",
            "content": [{"type": "text", "text": "legacy"}],
            "synthetic_reason": "doom_loop_warning"
        });
        let item: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::Unknown));
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn working_directory_switch_round_trips_generation() {
        let item = ConversationItem::working_directory_switch("moved", 7);
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["synthetic_reason"], "working_directory_switch");
        assert_eq!(json["cwd_generation"], 7);
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.working_directory_switch_generation(), Some(7));
    }

    #[test]
    fn legacy_user_defaults_cwd_generation_to_none() {
        let item: ConversationItem = serde_json::from_value(serde_json::json!({
            "type": "user",
            "content": [{"type": "text", "text": "hello"}],
            "synthetic_reason": "system_reminder"
        }))
        .expect("deserialize legacy user");
        let ConversationItem::User(user) = item else {
            panic!("expected user");
        };
        assert!(user.cwd_generation.is_none());
    }

    /// `synthetic_reason` round-trips through JSON.  Old sessions that omit
    /// the field entirely deserialize as `None` (via `#[serde(default)]`);
    /// sessions with the field preserve the value.
    #[test]
    fn synthetic_reason_json_roundtrip() {
        // New: has synthetic_reason.
        let item = ConversationItem::system_reminder("test");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(
            json["synthetic_reason"],
            serde_json::json!("system_reminder"),
            "synthetic_reason must serialize as snake_case string"
        );
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::SystemReminder));
        } else {
            panic!("expected User variant after round-trip");
        }

        // Old JSONL without the field must deserialize as None.
        let old_json = serde_json::json!({
            "type": "user",
            "content": [{"type": "text", "text": "hello"}]
        });
        let old: ConversationItem = serde_json::from_value(old_json).expect("deserialize old");
        if let ConversationItem::User(u) = old {
            assert!(
                u.synthetic_reason.is_none(),
                "old sessions without synthetic_reason must deserialize as None"
            );
        } else {
            panic!("expected User variant for old JSON");
        }
    }

    /// Real user messages must NOT serialize the `synthetic_reason` key at all
    /// (ensured by `skip_serializing_if = "Option::is_none"`).
    #[test]
    fn real_user_message_omits_synthetic_reason_key() {
        let item = ConversationItem::user("hello");
        let json = serde_json::to_value(&item).expect("serialize");
        assert!(
            json.get("synthetic_reason").is_none(),
            "real user messages must not include synthetic_reason in JSON"
        );
    }

    // -----------------------------------------------------------------------
    // user_meta / CompactionMeta tests
    // -----------------------------------------------------------------------

    #[test]
    fn user_meta_tagged_correctly() {
        let item = ConversationItem::user_meta("file contents here");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::CompactionMeta));
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn user_meta_content_preserved() {
        let text = "  1→use jwt::Claims;";
        let item = ConversationItem::user_meta(text);
        assert_eq!(item.text_content(), text);
    }

    #[test]
    fn user_meta_serde_roundtrip() {
        let item = ConversationItem::user_meta("test");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(
            json["synthetic_reason"],
            serde_json::json!("compaction_meta")
        );
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::CompactionMeta));
        } else {
            panic!("expected User variant after round-trip");
        }
    }

    #[test]
    fn system_reminder_tagged_correctly() {
        let item = ConversationItem::system_reminder("<system-reminder>test</system-reminder>");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::SystemReminder));
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn system_reminder_serde_roundtrip() {
        let item = ConversationItem::system_reminder("test");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(
            json["synthetic_reason"],
            serde_json::json!("system_reminder")
        );
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::SystemReminder));
        } else {
            panic!("expected User variant after round-trip");
        }
    }

    #[test]
    fn auto_continue_tagged_correctly() {
        let item = ConversationItem::auto_continue("keep going");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::AutoContinue));
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn auto_continue_serde_roundtrip() {
        let item = ConversationItem::auto_continue("keep going");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["synthetic_reason"], serde_json::json!("auto_continue"));
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::AutoContinue));
        } else {
            panic!("expected User variant after round-trip");
        }
    }

    #[test]
    fn auto_recovery_tagged_correctly() {
        let item = ConversationItem::auto_recovery("try again");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::AutoRecovery));
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn auto_recovery_serde_roundtrip() {
        let item = ConversationItem::auto_recovery("try again");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["synthetic_reason"], serde_json::json!("auto_recovery"));
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::AutoRecovery));
        } else {
            panic!("expected User variant after round-trip");
        }
    }

    /// `project_instructions` must be tagged with `ProjectInstructions` and
    /// preserve the text intact in a single `ContentPart::Text` part.
    #[test]
    fn project_instructions_tagged_correctly() {
        let item = ConversationItem::project_instructions("foo");
        if let ConversationItem::User(u) = item {
            assert_eq!(
                u.synthetic_reason,
                Some(SyntheticReason::ProjectInstructions),
                "project_instructions must carry SyntheticReason::ProjectInstructions"
            );
            match u.content.as_slice() {
                [ContentPart::Text { text }] => {
                    assert_eq!(
                        text.as_ref(),
                        "foo",
                        "text content must be preserved verbatim"
                    );
                }
                other => panic!("expected single text part, got {other:?}"),
            }
        } else {
            panic!("expected User variant");
        }
    }

    /// The text content of a project-instructions message is preserved
    /// unchanged through `text_content()`.
    #[test]
    fn project_instructions_content_preserved() {
        let text = "# AGENTS.md\n\nProject conventions go here.";
        let item = ConversationItem::project_instructions(text);
        assert_eq!(item.text_content(), text);
    }

    /// Serializes to snake_case `"project_instructions"` and round-trips
    /// back to the same variant.
    #[test]
    fn project_instructions_serde_roundtrip() {
        let item = ConversationItem::project_instructions("AGENTS.md body");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(
            json["synthetic_reason"],
            serde_json::json!("project_instructions"),
            "synthetic_reason must serialize as snake_case string"
        );
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        if let ConversationItem::User(u) = back {
            assert_eq!(
                u.synthetic_reason,
                Some(SyntheticReason::ProjectInstructions)
            );
        } else {
            panic!("expected User variant after round-trip");
        }
    }

    #[test]
    fn agent_message_reason_round_trips() {
        let item = ConversationItem::agent_message("agent context");
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["synthetic_reason"], "agent_message");
        let back: ConversationItem = serde_json::from_value(json).expect("deserialize");
        assert!(matches!(
            back,
            ConversationItem::User(UserItem {
                synthetic_reason: Some(SyntheticReason::AgentMessage),
                ..
            })
        ));
    }

    #[test]
    fn parent_agent_message_alias_deserializes() {
        let payload = serde_json::json!({
            "type": "user",
            "content": [{"type": "text", "text": "agent context"}],
            "synthetic_reason": "parent_agent_message"
        });
        let item: ConversationItem =
            serde_json::from_value(payload).expect("deserialize staged spelling");
        assert!(matches!(
            item,
            ConversationItem::User(UserItem {
                synthetic_reason: Some(SyntheticReason::AgentMessage),
                ..
            })
        ));
    }

    /// Forward-compat regression guard for the `#[serde(other)]` arm:
    /// payloads from newer clients with an unknown `synthetic_reason` value
    /// must deserialize as `Some(SyntheticReason::Unknown)` rather than
    /// failing.
    #[test]
    fn unknown_synthetic_reason_deserializes_for_forward_compat() {
        let payload = serde_json::json!({
            "type": "user",
            "content": [{"type": "text", "text": "hello"}],
            "synthetic_reason": "some_future_variant"
        });
        let item: ConversationItem =
            serde_json::from_value(payload).expect("deserialize forward-compat payload");
        if let ConversationItem::User(u) = item {
            assert_eq!(u.synthetic_reason, Some(SyntheticReason::Unknown));
            assert!(u.synthetic_reason.as_ref().unwrap().starts_prompt_turn());
        } else {
            panic!("expected User variant");
        }
    }

    #[test]
    fn empty_reason_none_when_has_content() {
        let resp = make_response(ConversationItem::assistant("hello"));
        assert!(resp.empty_reason().is_none());
        assert!(!resp.is_empty());
    }

    #[test]
    fn empty_reason_none_when_has_tool_calls() {
        let resp = make_response(ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "tc1".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }]));
        assert!(resp.empty_reason().is_none());
        assert!(!resp.is_empty());
    }

    #[test]
    fn empty_reason_no_visible_content() {
        let resp = make_response(ConversationItem::assistant(""));
        assert_eq!(
            resp.empty_reason(),
            Some(crate::error::EmptyReason::NoVisibleContent)
        );
        assert!(resp.is_empty());
    }

    #[test]
    fn empty_reason_non_assistant_returns_no_visible_content() {
        let resp = make_response(ConversationItem::tool_result("tc1", "result"));
        assert_eq!(
            resp.empty_reason(),
            Some(crate::error::EmptyReason::NoVisibleContent)
        );
    }

    #[test]
    fn stop_reason_as_str_matches_serde() {
        assert_eq!(StopReason::Stop.as_str(), "stop");
        assert_eq!(StopReason::Length.as_str(), "length");
        assert_eq!(StopReason::ToolCalls.as_str(), "tool_calls");
        assert_eq!(StopReason::ContentFilter.as_str(), "content_filter");
    }

    // ============================================================================
    // Reasoning-as-sibling regression tests
    //
    // These pin the invariants that motivated this refactor:
    //
    // 1. `tco_*` reasoning items from parallel backend tool calls round-trip
    //    losslessly as N sibling `Reasoning` items (a prior data-loss
    //    bug — was last-write-wins on `AssistantItem.reasoning`).
    //
    // 2. Multi-turn conversations preserve emission order
    //    `[Sys, U1, R, BTC*, A1, U2, R, BTC*, A2, ...]` rather than
    //    `[Sys, U1, ..., UN, R*, A*]` (a prior ordering bug that
    //    torched the server-side prefix cache).
    //
    // 3. `conversation_to_chat_messages` folds preceding Reasoning siblings
    //    into the next assistant's `reasoning_content` for the
    //    chat-completions wire path.
    //
    // 4. `patch_reasoning_text_types` injects the `type: "reasoning_text"`
    //    discriminator on nested `content[]` items that async-openai's
    //    derived Serialize omits.
    // ============================================================================

    #[test]
    fn multi_tco_reasoning_items_round_trip_as_siblings() {
        let make_reasoning = |suffix: &str, summary: &str, encrypted: Option<&str>| {
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: format!("rs_resp123_{suffix}"),
                summary: if summary.is_empty() {
                    vec![]
                } else {
                    vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                        text: summary.to_string(),
                    })]
                },
                content: None,
                encrypted_content: encrypted.map(str::to_owned),
                status: Some(rs::OutputStatus::Completed),
            })
        };
        let make_tco = |suffix: &str| {
            rs::OutputItem::Reasoning(rs::ReasoningItem {
                id: format!("tco_resp123_call-{suffix}"),
                summary: vec![],
                content: None,
                encrypted_content: Some(format!("enc_blob_{suffix}")),
                status: Some(rs::OutputStatus::Completed),
            })
        };
        let make_ws = |suffix: &str, query: &str| {
            rs::OutputItem::WebSearchCall(rs::WebSearchToolCall {
                id: format!("ws_resp123_{suffix}"),
                status: rs::WebSearchToolCallStatus::Completed,
                action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                    query: query.to_string(),
                    sources: Some(vec![]),
                }),
            })
        };

        let response = rs::Response {
            background: None,
            billing: None,
            conversation: None,
            created_at: 0,
            completed_at: None,
            error: None,
            id: "resp123".to_string(),
            incomplete_details: None,
            instructions: None,
            max_output_tokens: None,
            metadata: None,
            model: "echo-agent-build".to_string(),
            object: "response".to_string(),
            output: vec![
                make_reasoning("a", "thinking pre-search", None),
                make_ws("5", "capybara facts"),
                make_ws("6", "wombat habitat"),
                make_tco("5"),
                make_tco("6"),
                make_reasoning("b", "follow-up thinking", None),
                make_ws("7", "platypus venom"),
                make_tco("7"),
            ],
            parallel_tool_calls: None,
            previous_response_id: None,
            prompt: None,
            prompt_cache_key: None,
            prompt_cache_retention: None,
            reasoning: None,
            safety_identifier: None,
            service_tier: None,
            status: rs::Status::Completed,
            temperature: None,
            text: None,
            tool_choice: None,
            tools: None,
            top_logprobs: None,
            top_p: None,
            truncation: None,
            usage: None,
        };

        let items = response_to_conversation_items(response);

        // Five reasoning siblings: 2 real `rs_*` + 3 encrypted `tco_*`.
        let reasoning_ids: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                ConversationItem::Reasoning(r) => Some(r.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasoning_ids,
            vec![
                "rs_resp123_a",
                "tco_resp123_call-5",
                "tco_resp123_call-6",
                "rs_resp123_b",
                "tco_resp123_call-7",
            ],
            "every reasoning item — including all 3 tco_* — must round-trip in emission order"
        );

        // Exactly one trailing Assistant.
        assert!(matches!(items.last(), Some(ConversationItem::Assistant(_))));

        // Backend tool calls preserved in order.
        let bt_count = items
            .iter()
            .filter(|i| matches!(i, ConversationItem::BackendToolCall(_)))
            .count();
        assert_eq!(bt_count, 3);
    }

    #[test]
    fn conversation_to_chat_messages_folds_reasoning_into_following_assistant() {
        let items = vec![
            ConversationItem::user("hi"),
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r1".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "thinking step 1".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }),
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r2".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "thinking step 2".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }),
            ConversationItem::assistant("answer"),
        ];

        let msgs = conversation_to_chat_messages(items);
        assert_eq!(msgs.len(), 2, "user + assistant; reasoning items folded");
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].text_content(), "answer");
        assert_eq!(
            msgs[1].reasoning_content.as_deref(),
            Some("thinking step 1\nthinking step 2"),
            "reasoning text joined and attached to the assistant"
        );
    }

    #[test]
    fn conversation_to_chat_messages_drops_trailing_reasoning() {
        let items = vec![
            ConversationItem::user("hi"),
            ConversationItem::Reasoning(rs::ReasoningItem {
                id: "r1".to_string(),
                summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: "abandoned thinking".to_string(),
                })],
                content: None,
                encrypted_content: None,
                status: None,
            }),
        ];
        let msgs = conversation_to_chat_messages(items);
        assert_eq!(
            msgs.len(),
            1,
            "trailing reasoning has no assistant to attach to"
        );
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn conversation_to_chat_messages_folds_reasoning_across_backend_tool_call() {
        // The canonical post-tool-call ordering is
        // `[..., Reasoning, BackendToolCall, Assistant]` (e.g. a web_search
        // turn). The BackendToolCall is emitted as its own synthetic assistant
        // message, but it must NOT drop the pending reasoning: the reasoning
        // belongs to the same turn and folds onto the following assistant's
        // `reasoning_content`, matching the Responses API path
        // (`build_responses_input_preserves_multi_turn_ordering`).
        let items = vec![
            ConversationItem::user("hi"),
            reasoning_sibling("r1", "thinking before search", None),
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
                    id: "ws_1".to_string(),
                    status: rs::WebSearchToolCallStatus::Completed,
                    action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                        query: "capybaras".to_string(),
                        sources: Some(vec![]),
                    }),
                }),
            }),
            ConversationItem::assistant("answer"),
        ];

        let msgs = conversation_to_chat_messages(items);

        assert_eq!(msgs.len(), 3, "user + synthetic BTC assistant + assistant");
        assert_eq!(msgs[0].role, Role::User);
        // BackendToolCall becomes a synthetic assistant carrying its summary;
        // it does not itself carry the reasoning.
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(
            msgs[1].text_content(),
            "[backend web_search] search: capybaras"
        );
        assert_eq!(
            msgs[1].reasoning_content.as_deref(),
            None,
            "reasoning lands on the real assistant, not the synthetic BTC message"
        );
        // The real assistant turn keeps the reasoning that preceded the
        // backend tool call.
        assert_eq!(msgs[2].role, Role::Assistant);
        assert_eq!(msgs[2].text_content(), "answer");
        assert_eq!(
            msgs[2].reasoning_content.as_deref(),
            Some("thinking before search"),
            "reasoning preceding a BackendToolCall folds onto the following \
             assistant rather than being dropped"
        );
    }

    #[test]
    fn conversation_item_to_chat_message_backend_tool_call_is_synthetic_assistant() {
        // The only conversion arm with no direct unit test: a BackendToolCall
        // has no Chat Completions equivalent, so it is emitted as a synthetic
        // assistant message carrying its human-readable `text_summary()`.
        let item = ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
                id: "ws_1".to_string(),
                status: rs::WebSearchToolCallStatus::Completed,
                action: rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                    query: "capybaras".to_string(),
                    sources: Some(vec![]),
                }),
            }),
        });

        let msg = conversation_item_to_chat_message(item);

        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.text_content(), "[backend web_search] search: capybaras");
        assert!(
            msg.tool_calls.is_empty(),
            "synthetic assistant carries no tool calls"
        );
        assert_eq!(msg.reasoning_content.as_deref(), None);
    }

    // ========================================================================
    // upgrade_legacy_reasoning — legacy in-memory reconstruction
    // ========================================================================
    //
    // Three legacy on-disk shapes that the on-read upgrader must lift to
    // sibling Reasoning / BackendToolCall items:
    //
    //   1. v1 assistant with `raw_output: Vec<OutputItem>` (backend-search era)
    //   2. v1 assistant with singular `reasoning: ReasoningContent`
    //      (earlier echo-agent-build / chat-completions written as v1)
    //   3. v0 `ChatRequestMessage` with top-level `reasoning_content`
    //
    // Idempotent (current-format rows produce zero siblings) — verified by
    // `upgrade_is_idempotent_on_post_pr_rows`.

    #[test]
    fn upgrade_legacy_reasoning_singular_echo_agent_build_shape() {
        // Synthetic fixture (truncated text + encrypted stub)
        // (truncated text + encrypted for readability). The assistant row
        // carries `reasoning: { text, encrypted, id }` inline — the
        // earlier shape.
        let raw = serde_json::json!({
            "type": "assistant",
            "content": "Web search results for cats and dogs...",
            "reasoning": {
                "text": "The web search results for cats and dogs are mostly about",
                "encrypted": "bIfXFNBiP8EI8F7pkKC1tgbYjvVuIctMAlCUGMii",
                "id": "rs_00000000-0000-4000-8000-000000000001"
            },
            "model_id": "echo-agent-build",
            "model_fingerprint": "fp_test000000000001"
        });
        let mut seen = std::collections::HashSet::new();
        let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
        assert_eq!(siblings.len(), 1, "exactly one sibling Reasoning emitted");
        let ConversationItem::Reasoning(r) = &siblings[0] else {
            panic!("expected Reasoning sibling, got {:?}", siblings[0]);
        };
        assert_eq!(r.id, "rs_00000000-0000-4000-8000-000000000001");
        assert_eq!(r.summary.len(), 1);
        let rs::SummaryPart::SummaryText(s) = &r.summary[0];
        assert_eq!(
            s.text,
            "The web search results for cats and dogs are mostly about"
        );
        assert_eq!(
            r.encrypted_content.as_deref(),
            Some("bIfXFNBiP8EI8F7pkKC1tgbYjvVuIctMAlCUGMii")
        );
    }

    #[test]
    fn upgrade_legacy_reasoning_raw_output_expands_parallel_tco_blobs() {
        // backend-search-era shape: raw_output preserves the full ordered
        // Vec<OutputItem>. The N parallel `tco_*` reasoning items round-
        // trip as N sibling Reasoning items — the structural fix the
        // refactor is built around.
        let raw = serde_json::json!({
            "type": "assistant",
            "content": "I searched two things in parallel.",
            "tool_calls": [],
            "raw_output": [
                {"type":"reasoning","id":"tco_1","summary":[],"encrypted_content":"enc1"},
                {"type":"web_search_call","id":"ws_1","status":"completed",
                 "action":{"type":"search","query":"q1","sources":[]}},
                {"type":"reasoning","id":"tco_2","summary":[],"encrypted_content":"enc2"},
                {"type":"web_search_call","id":"ws_2","status":"completed",
                 "action":{"type":"search","query":"q2","sources":[]}},
                {"type":"reasoning","id":"rs_main",
                 "summary":[{"type":"summary_text","text":"final synthesis"}]},
                {"type":"message","id":"msg_1","status":"completed","role":"assistant",
                 "content":[{"type":"output_text","text":"I searched two things in parallel.",
                             "annotations":[]}]}
            ]
        });
        let mut seen = std::collections::HashSet::new();
        let siblings = upgrade_legacy_reasoning(&raw, &mut seen);

        // 3 Reasoning + 2 BackendToolCall = 5 siblings.
        // Message and FunctionCall (none here) are NOT emitted as siblings.
        assert_eq!(siblings.len(), 5);

        let reasoning_ids: Vec<&str> = siblings
            .iter()
            .filter_map(|s| match s {
                ConversationItem::Reasoning(r) => Some(r.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reasoning_ids,
            vec!["tco_1", "tco_2", "rs_main"],
            "all three reasoning items recovered in emission order — duplicate recovery is \
             structurally impossible here"
        );

        let btc_ids: Vec<&str> = siblings
            .iter()
            .filter_map(|s| match s {
                ConversationItem::BackendToolCall(b) => Some(b.id()),
                _ => None,
            })
            .collect();
        assert_eq!(btc_ids, vec!["ws_1", "ws_2"]);

        // Both web-search ids registered for downstream dedup.
        assert!(seen.contains("ws_1"));
        assert!(seen.contains("ws_2"));
    }

    #[test]
    fn upgrade_legacy_reasoning_dedupes_backend_tool_calls_seen_as_siblings() {
        // BackendToolCall was already a sibling in legacy rows, so the same
        // call can appear *both* as its own JSONL row *and* inside the
        // following assistant's raw_output. The upgrader must not emit
        // a duplicate.
        let mut seen = std::collections::HashSet::new();
        seen.insert("ws_already_a_sibling".to_string());

        let raw = serde_json::json!({
            "type": "assistant",
            "content": "",
            "raw_output": [
                {"type":"web_search_call","id":"ws_already_a_sibling","status":"completed",
                 "action":{"type":"search","query":"x","sources":[]}},
                {"type":"web_search_call","id":"ws_new","status":"completed",
                 "action":{"type":"search","query":"y","sources":[]}}
            ]
        });
        let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
        let btc_ids: Vec<&str> = siblings
            .iter()
            .filter_map(|s| match s {
                ConversationItem::BackendToolCall(b) => Some(b.id()),
                _ => None,
            })
            .collect();
        assert_eq!(
            btc_ids,
            vec!["ws_new"],
            "the already-sibling call is skipped; only the new one is emitted"
        );
    }

    #[test]
    fn upgrade_is_idempotent_on_post_pr_rows() {
        // Current-format assistant has neither `reasoning` nor `raw_output`;
        // upgrader must produce zero siblings (so re-running the load
        // path doesn't accumulate duplicates).
        let raw = serde_json::json!({
            "type": "assistant",
            "content": "answer",
            "model_id": "echo-agent-build"
        });
        let mut seen = std::collections::HashSet::new();
        assert!(upgrade_legacy_reasoning(&raw, &mut seen).is_empty());

        // Standalone sibling rows are also no-ops.
        let r = serde_json::json!({"type":"reasoning","id":"rs_1","summary":[]});
        assert!(upgrade_legacy_reasoning(&r, &mut seen).is_empty());

        let u = serde_json::json!({"type":"user","content":[{"type":"text","text":"hi"}]});
        assert!(upgrade_legacy_reasoning(&u, &mut seen).is_empty());

        let s = serde_json::json!({"type":"system","content":"prompt"});
        assert!(upgrade_legacy_reasoning(&s, &mut seen).is_empty());
    }

    #[test]
    fn upgrade_skips_assistant_with_empty_reasoning() {
        // Assistant with `reasoning: {}` (no text / encrypted / id) — no
        // sibling should be emitted; there's nothing to preserve.
        let raw = serde_json::json!({
            "type": "assistant",
            "content": "answer",
            "reasoning": {}
        });
        let mut seen = std::collections::HashSet::new();
        assert!(upgrade_legacy_reasoning(&raw, &mut seen).is_empty());
    }

    /// INVARIANT: prefix stability across turns without reasoning.
    /// Context and instructions must be prefixed once and consistently
    /// across requests.
    #[test]
    fn prefix_stable_across_turns_no_reasoning() {
        let turn1_items = vec![
            ConversationItem::system("You are a helpful assistant."),
            ConversationItem::user("Hello"),
        ];
        let req1 = ConversationRequest::from_items(turn1_items.clone());

        let mut turn2_items = turn1_items.clone();
        turn2_items.push(ConversationItem::assistant("Hi there!"));
        turn2_items.push(ConversationItem::user("How are you?"));
        let req2 = ConversationRequest::from_items(turn2_items.clone());

        assert_prefix_stable(&req1, &req2);

        let mut turn3_items = turn2_items.clone();
        turn3_items.push(ConversationItem::assistant("I'm well!"));
        turn3_items.push(ConversationItem::tool_result("tc1", "x"));
        turn3_items.push(ConversationItem::user("Great"));
        let req3 = ConversationRequest::from_items(turn3_items);

        assert_prefix_stable(&req2, &req3);
    }

    /// Prefix stability when turns carry Reasoning siblings with
    /// encrypted_content. Encrypted reasoning is the cache-sensitive payload --
    /// its serialized position must be byte-identical across turns or
    /// the server-side prefix cache misses.
    #[test]
    fn prefix_stable_with_reasoning_siblings() {
        let turn1 = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("u1"),
        ];
        let req1 = ConversationRequest::from_items(turn1.clone());

        let mut turn2 = turn1.clone();
        turn2.push(reasoning_sibling("r1", "thinking 1", Some("enc1")));
        turn2.push(ConversationItem::assistant("response 1"));
        turn2.push(ConversationItem::user("u2"));
        let req2 = ConversationRequest::from_items(turn2.clone());

        assert_prefix_stable(&req1, &req2);

        let mut turn3 = turn2.clone();
        turn3.push(reasoning_sibling("r2", "thinking 2", Some("enc2")));
        turn3.push(ConversationItem::assistant("response 2"));
        turn3.push(ConversationItem::user("u3"));
        let req3 = ConversationRequest::from_items(turn3);

        assert_prefix_stable(&req2, &req3);
    }

    /// Canary for `serde_json`'s `preserve_order` feature.
    ///
    /// With `preserve_order` (Cargo.toml), `serde_json::Map` is backed by
    /// `IndexMap` which preserves insertion (struct-declaration) order.
    /// Without it, `BTreeMap` is used which alphabetizes keys. This test
    /// serializes once and verifies that a known field ordering matches
    /// the struct declaration order (not alphabetical). If the feature
    /// is accidentally removed from Cargo.toml, the assertion will fail.
    #[test]
    fn serialization_determinism() {
        let req = ConversationRequest::from_items(vec![
            ConversationItem::system("You are a helpful assistant."),
            ConversationItem::user("Hello, how are you?"),
            reasoning_sibling("r1", "think", Some("enc")),
            ConversationItem::assistant("I'm well!"),
            ConversationItem::user("Tell me more."),
        ]);

        // Deterministic: same input -> same bytes.
        let body1 = serde_json::to_string(&input_items_json(&req)).unwrap();
        let body2 = serde_json::to_string(&input_items_json(&req)).unwrap();
        assert_eq!(body1, body2, "repeated serialization must be identical");

        // Insertion-order preservation: for an EasyInputMessage with
        // serde tag = "type" (renamed to snake_case), the wire JSON must
        // emit `type` before `role` before `content`. With BTreeMap
        // (no preserve_order) these would be alphabetized to
        // content, role, type.
        let input = input_items_json(&req);
        let first_item_str = serde_json::to_string(&input[0]).unwrap();
        let type_pos = first_item_str
            .find("\"type\"")
            .expect("type field must exist");
        let role_pos = first_item_str
            .find("\"role\"")
            .expect("role field must exist");
        let content_pos = first_item_str
            .find("\"content\"")
            .expect("content field must exist");
        assert!(
            type_pos < role_pos && role_pos < content_pos,
            "preserve_order must maintain struct declaration order \
             (type < role < content), got type@{type_pos} role@{role_pos} \
             content@{content_pos}. Is `preserve_order` enabled in Cargo.toml?"
        );
    }

    /// Reasoning sibling WITHOUT `encrypted_content` (e.g. synthesized
    /// from Chat Completions plaintext `reasoning_content`) still
    /// round-trips inline -- there is no "fast path" or "slow path",
    /// just typed serialization. Replaces the old
    /// `test_reasoning_without_encrypted_content_no_placeholder`.
    #[test]
    fn reasoning_without_encrypted_content_round_trips_inline() {
        let req = ConversationRequest::from_items(vec![
            ConversationItem::system("sys"),
            ConversationItem::user("u1"),
            reasoning_sibling("r1", "I think about this...", None),
            ConversationItem::assistant("response text"),
        ]);

        let input = input_items_json(&req);
        let summary = summarise_input(&input);

        assert_eq!(summary.len(), 4);
        assert_eq!(summary[2], "reasoning:r1");

        // Encrypted content absent on the wire.
        assert!(
            input[2].get("encrypted_content").is_none()
                || input[2]
                    .get("encrypted_content")
                    .and_then(|v| v.as_str())
                    .is_none(),
        );

        // The placeholder sentinel from the pre-refactor world must not appear.
        let body_str = serde_json::to_string(&input).unwrap();
        assert!(!body_str.contains("__RAW_OUTPUT_PLACEHOLDER_"));
    }
}
