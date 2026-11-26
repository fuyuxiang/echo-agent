//! The `streaming-messages-json` serde wire DTOs: the Messages API line shapes
//! plus their serialization helpers. Pure data; reducer logic lives elsewhere.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::headless::reducer::McpServer;
use echo_agent_runtime::extensions::notification::ResponseUsage;

pub(super) fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Serialize an `f64` cost, substituting `0.0` for a non-finite value that would
/// otherwise make `serde_json` error and degrade the whole line to the `error` fallback.
pub(super) fn serialize_finite_cost<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(if value.is_finite() { *value } else { 0.0 })
}

/// Map a EchoAgent permission mode to the Messages `permissionMode` enum, clamping EchoAgent-only values to `default`.
pub(super) fn messages_permission_mode(mode: Option<&str>) -> &'static str {
    match mode {
        Some("acceptEdits") => "acceptEdits",
        Some("bypassPermissions") => "bypassPermissions",
        Some("plan") => "plan",
        Some("dontAsk") => "dontAsk",
        _ => "default",
    }
}

/// A block in a Messages API `assistant.message.content[]`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// A backend `web_search` folded inline as Messages API `server_tool_use`.
    ServerToolUse {
        id: String,
        name: &'static str,
        input: Value,
    },
    /// The results of an inline web search (`web_search_tool_result`); `content` is the hit array.
    WebSearchToolResult {
        tool_use_id: String,
        content: Value,
    },
}

/// The wire fields of Messages API `message.usage` (reasoning tokens folded into `output_tokens`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct MessageUsage {
    #[serde(default)]
    pub(super) input_tokens: u64,
    #[serde(default)]
    pub(super) output_tokens: u64,
    #[serde(default)]
    pub(super) cache_read_input_tokens: u64,
    #[serde(default)]
    pub(super) cache_creation_input_tokens: u64,
    /// The `usage.server_tool_use` counter; populated only on the terminal `result` usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) server_tool_use: Option<ServerToolUse>,
}

/// The `usage.server_tool_use` sub-object (EchoAgent counts backend `web_search` only).
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct ServerToolUse {
    pub(super) web_search_requests: u64,
}

impl From<&ResponseUsage> for MessageUsage {
    /// Project the shell's per-response usage onto the four `message.usage` fields.
    fn from(u: &ResponseUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            // Server-tool counts ride the terminal `result` usage only.
            server_tool_use: None,
        }
    }
}

#[derive(Serialize)]
pub(super) struct AssistantFrame {
    pub(super) message: AssistantMessage,
    pub(super) parent_tool_use_id: Option<String>,
    pub(super) session_id: String,
    pub(super) uuid: String,
}

#[derive(Serialize)]
pub(super) struct AssistantMessage {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) role: &'static str,
    pub(super) model: String,
    pub(super) content: Vec<ContentBlock>,
    // Nullable so a turn that ended abnormally reports `null`, not a misleading `end_turn`.
    pub(super) stop_reason: Option<String>,
    pub(super) stop_sequence: Option<String>,
    pub(super) usage: MessageUsage,
}

/// The Anthropic Messages API `system`/`init` line body (`stream-json` shape).
#[derive(Serialize)]
pub(super) struct SystemInitLine {
    pub(super) session_id: String,
    #[serde(rename = "apiKeySource")]
    pub(super) api_key_source: &'static str,
    pub(super) model: String,
    pub(super) cwd: String,
    #[serde(rename = "permissionMode")]
    pub(super) permission_mode: &'static str,
    pub(super) tools: Vec<String>,
    pub(super) slash_commands: Vec<String>,
    pub(super) mcp_servers: Vec<McpServer>,
    pub(super) skills: Vec<String>,
    pub(super) uuid: String,
}

/// The Messages API `stream_event` partial; `event` is an opaque raw stream event.
#[derive(Serialize)]
pub(super) struct PartialEventLine {
    pub(super) event: Value,
    pub(super) parent_tool_use_id: Option<String>,
    pub(super) session_id: String,
    pub(super) uuid: String,
}

/// The Messages API `user`/`tool_result` line body.
#[derive(Serialize)]
pub(super) struct ToolResultLine {
    pub(super) message: ToolResultMessage,
    pub(super) parent_tool_use_id: Option<String>,
    pub(super) session_id: String,
    pub(super) uuid: String,
}

#[derive(Serialize)]
pub(super) struct ToolResultMessage {
    pub(super) role: &'static str,
    pub(super) content: Vec<ToolResultBlock>,
}

#[derive(Serialize)]
pub(super) struct ToolResultBlock {
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) tool_use_id: String,
    pub(super) content: Value,
    pub(super) is_error: bool,
}

/// The Messages API `system`/`compact_boundary` line body.
#[derive(Serialize)]
pub(super) struct CompactBoundaryLine {
    pub(super) compact_metadata: CompactMetadata,
    pub(super) session_id: String,
    pub(super) uuid: String,
}

#[derive(Serialize)]
pub(super) struct CompactMetadata {
    pub(super) trigger: &'static str,
    pub(super) pre_tokens: u64,
}

/// Every top-level `streaming-messages-json` NDJSON line. `#[serde(tag = "type")]`
/// derives the `type` discriminant from the variant, so a line can never carry a
/// mistyped tag; the two `system` subtypes ride the nested [`SystemLine`] `subtype`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum MessagesLine {
    System(SystemLine),
    Assistant(AssistantFrame),
    User(ToolResultLine),
    StreamEvent(PartialEventLine),
    Result(Box<ResultLine>),
}

/// The two `system` line subtypes, discriminated by `subtype`.
#[derive(Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub(super) enum SystemLine {
    Init(SystemInitLine),
    CompactBoundary(CompactBoundaryLine),
}

#[derive(Serialize)]
pub(super) struct ModelUsage {
    #[serde(rename = "inputTokens")]
    pub(super) input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub(super) output_tokens: u64,
    #[serde(rename = "cacheReadInputTokens")]
    pub(super) cache_read_input_tokens: u64,
    #[serde(rename = "cacheCreationInputTokens")]
    pub(super) cache_creation_input_tokens: u64,
    #[serde(rename = "webSearchRequests")]
    pub(super) web_search_requests: u64,
    #[serde(rename = "costUSD", serialize_with = "serialize_finite_cost")]
    pub(super) cost_usd: f64,
    #[serde(rename = "contextWindow", skip_serializing_if = "Option::is_none")]
    pub(super) context_window: Option<u64>,
}

/// The Messages API terminal `result` line body (success and error subtypes).
#[derive(Serialize)]
pub(super) struct ResultLine {
    pub(super) subtype: &'static str,
    pub(super) is_error: bool,
    pub(super) duration_ms: u64,
    pub(super) duration_api_ms: u64,
    pub(super) num_turns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<String>,
    pub(super) stop_reason: Option<String>,
    #[serde(serialize_with = "serialize_finite_cost")]
    pub(super) total_cost_usd: f64,
    pub(super) usage: MessageUsage,
    /// Per-model usage keyed by model id; `{}` when there is no per-model breakdown.
    #[serde(rename = "modelUsage")]
    pub(super) model_usage: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) structured_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) errors: Option<Vec<String>>,
    pub(super) session_id: String,
    pub(super) uuid: String,
}

/// Raw Messages API stream events serialized as the `event` body of a `stream_event` line.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum StreamEventBody {
    MessageStart {
        message: PartialMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: PartialBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: PartialDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        usage: MessageUsage,
    },
    MessageStop,
}

/// The `message` body of a partial `message_start`; carries the real id and
/// input-side usage, with `output_tokens` finalized later on `message_delta`.
#[derive(Serialize)]
pub(super) struct PartialMessage {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) role: &'static str,
    pub(super) model: String,
    pub(super) content: Vec<PartialBlock>,
    pub(super) stop_reason: Option<String>,
    pub(super) stop_sequence: Option<String>,
    pub(super) usage: MessageUsage,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PartialBlock {
    Text {
        text: &'static str,
    },
    Thinking {
        thinking: &'static str,
        signature: &'static str,
    },
    ToolUse {
        id: String,
        name: String,
        input: EmptyObject,
    },
    /// Partial-stream open for a `server_tool_use` block; input rides a following `input_json_delta`.
    ServerToolUse {
        id: String,
        name: &'static str,
        input: EmptyObject,
    },
    /// Partial-stream `web_search_tool_result` block; carries its full `content` at start.
    WebSearchToolResult {
        tool_use_id: String,
        content: Value,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PartialDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}

#[derive(Serialize)]
pub(super) struct MessageDeltaBody {
    pub(super) stop_reason: Option<String>,
    pub(super) stop_sequence: Option<String>,
}

/// Serializes to `{}` (the empty initial `tool_use.input`).
#[derive(Serialize)]
pub(super) struct EmptyObject {}
