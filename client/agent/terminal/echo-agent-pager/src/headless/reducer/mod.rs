//! Streaming reducers that fold the agent ACP event stream into NDJSON lines.
//! One `Reducer` impl per wire format, selected by [`reducer_for`]. This module
//! owns the shared [`StreamEvent`]/[`Reducer`] types; each format is a submodule.

use agent_client_protocol as proto;
use serde::Serialize;
use serde_json::{Value, json};

use crate::headless::OutputFormat;

mod acp;
mod messages;

use self::acp::AcpReducer;
use self::messages::MessagesReducer;
use echo_agent_runtime::extensions::notification::ResponseUsage;

/// Serialize a wire line, degrading to an `error` line rather than panicking.
pub(crate) fn to_line<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value)
        .unwrap_or_else(|e| json!({"type": "error", "message": format!("serialize failed: {e}")}))
}

/// Merge the camelCase `structuredOutput`/`structuredOutputError` fields into a
/// terminal wire line: `Ok` stamps the value, `Err` stamps null plus the error.
pub(crate) fn attach_structured_output(
    target: &mut Value,
    structured: Option<Result<Value, String>>,
) {
    match structured {
        None => {}
        Some(Ok(value)) => target["structuredOutput"] = value,
        Some(Err(err)) => {
            target["structuredOutput"] = Value::Null;
            target["structuredOutputError"] = err.into();
        }
    }
}

/// Transport agnostic agent stream event, the single input every reducer folds.
pub(crate) enum StreamEvent {
    AgentMessage(String),
    AgentThought(String),
    ToolCall(ToolCallEvent),
    ToolCallUpdate(ToolCallUpdateEvent),
    Plan(Value),
    /// Session metadata; `skills` is the subset of `commands` that are skills.
    AvailableCommands {
        tools: Vec<String>,
        commands: Vec<String>,
        skills: Vec<String>,
    },
    Lifecycle(Lifecycle),
    /// One model response opened (Messages backend); carries real id, model, and input-side token counts.
    ResponseStarted {
        message_id: Option<String>,
        model: Option<String>,
        input_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    },
    /// One reasoning block finished (Messages backend); carries its signature for in-order `signature_delta`.
    ReasoningCompleted {
        signature: Option<String>,
    },
    /// One model response finished; carries its stop reason, id, usage, signature, and stop sequence.
    ResponseCompleted {
        message_id: Option<String>,
        stop_reason: Option<String>,
        usage: Option<ResponseUsage>,
        signature: Option<String>,
        /// Provider's matched stop sequence; set only by the Messages reducer.
        stop_sequence: Option<String>,
    },
}

pub(crate) struct ToolCallEvent {
    tool_call_id: String,
    title: String,
    tool_kind: Option<String>,
    status: Option<proto::ToolCallStatus>,
    tool_name: String,
    raw_input: Value,
    content: Value,
    locations: Value,
    /// True for EchoAgent's backend `web_search`, which folds inline instead of the client split.
    backend_web_search: bool,
}

pub(crate) struct ToolCallUpdateEvent {
    tool_call_id: String,
    status: Option<proto::ToolCallStatus>,
    content: Value,
    raw_output: Value,
    locations: Value,
}

pub(crate) enum Lifecycle {
    CompactStarted { percentage: u8 },
    CompactCompleted { pre_tokens: u64 },
    CompactFailed { error: String },
    CompactCancelled,
    AutoContinue { total_tokens: u64 },
    ImageCompressed { message: String },
}

impl Lifecycle {
    /// The human-readable `plain` line for this lifecycle event.
    pub(crate) fn plain_message(&self) -> String {
        match self {
            Lifecycle::CompactStarted { percentage } => {
                format!("Auto-compacting conversation ({percentage}% full)...")
            }
            Lifecycle::CompactCompleted { .. } => "Conversation compacted.".to_string(),
            Lifecycle::CompactFailed { error } if error.trim().is_empty() => {
                "Auto-compact failed.".to_string()
            }
            Lifecycle::CompactFailed { error } => format!("Auto-compact failed: {error}"),
            Lifecycle::CompactCancelled => "Auto-compact cancelled.".to_string(),
            Lifecycle::AutoContinue { .. } => "Resumed after compaction.".to_string(),
            Lifecycle::ImageCompressed { message } => message.clone(),
        }
    }
}

/// The `streaming-json` wire token for an ACP [`proto::ToolKind`]. Explicit typed
/// match (not serde) so renaming a variant is a compile error, not a dropped `None`.
pub(crate) fn tool_kind_wire(kind: proto::ToolKind) -> Option<String> {
    let token = match kind {
        proto::ToolKind::Read => "read",
        proto::ToolKind::Edit => "edit",
        proto::ToolKind::Delete => "delete",
        proto::ToolKind::Move => "move",
        proto::ToolKind::Search => "search",
        proto::ToolKind::Execute => "execute",
        proto::ToolKind::Think => "think",
        proto::ToolKind::Fetch => "fetch",
        proto::ToolKind::SwitchMode => "switch_mode",
        proto::ToolKind::Other => "other",
        _ => return serde_wire_token(&kind),
    };
    Some(token.to_string())
}

/// The `streaming-json` wire token for an ACP [`proto::ToolCallStatus`] (see [`tool_kind_wire`]).
pub(crate) fn tool_call_status_wire(status: proto::ToolCallStatus) -> Option<String> {
    let token = match status {
        proto::ToolCallStatus::Pending => "pending",
        proto::ToolCallStatus::InProgress => "in_progress",
        proto::ToolCallStatus::Completed => "completed",
        proto::ToolCallStatus::Failed => "failed",
        _ => return serde_wire_token(&status),
    };
    Some(token.to_string())
}

/// Fallback wire token for a future `#[non_exhaustive]` ACP variant not covered above.
fn serde_wire_token<T: serde::Serialize>(value: &T) -> Option<String> {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// Serialize an ACP value expected to be a JSON array, degrading to `[]` on failure.
fn json_array_or_empty<T: serde::Serialize>(value: &T) -> Value {
    match serde_json::to_value(value) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "headless: failed to serialize ACP value; emitting []");
            json!([])
        }
    }
}

/// Canonical model-facing tool name from the `echo.agent/tool` `_meta` envelope, else
/// the display title, else kind, else `"tool"`. The shell stamps the wire name
/// (`bash`, `x_search`, `read_file`) under `echo.agent/tool.name`; without this the
/// name falls through to the human title (`Execute ...`, `X search:`).
fn tool_name_from(meta: Option<&proto::Meta>, title: &str, kind: Option<&str>) -> String {
    if let Some(meta) = meta
        && let Some(name) = meta
            .get("echo.agent/tool")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        && !name.is_empty()
    {
        return name.to_string();
    }
    if !title.is_empty() {
        return title.to_string();
    }
    if let Some(kind) = kind
        && !kind.is_empty()
    {
        return kind.to_string();
    }
    "tool".to_string()
}

/// True iff `tc` is EchoAgent's backend `web_search` (`_meta.backend` and `raw_input.variant == "WebSearch"`).
fn is_backend_web_search(meta: Option<&proto::Meta>, raw_input: &Value) -> bool {
    let backend = meta
        .and_then(|m| m.get("backend"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_web_search = raw_input.get("variant").and_then(Value::as_str) == Some("WebSearch");
    backend && is_web_search
}

/// Canonical tool kind from the `echo.agent/tool` `_meta` envelope, else the ACP
/// `ToolCall.kind`. Client tools register as `ToolKind::Other` on the early
/// notification, so the real kind (`read`, `edit`, `execute`) rides
/// `echo.agent/tool.kind`; without this every client tool reports `other`.
fn tool_kind_from(meta: Option<&proto::Meta>, kind: proto::ToolKind) -> Option<String> {
    if let Some(meta) = meta
        && let Some(k) = meta
            .get("echo.agent/tool")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str())
        && !k.is_empty()
    {
        return Some(k.to_string());
    }
    tool_kind_wire(kind)
}

pub(crate) fn tool_call_event(tc: &proto::ToolCall) -> ToolCallEvent {
    let tool_kind = tool_kind_from(tc.meta.as_ref(), tc.kind);
    let tool_name = tool_name_from(tc.meta.as_ref(), &tc.title, tool_kind.as_deref());
    let raw_input = tc.raw_input.clone().unwrap_or(Value::Null);
    let backend_web_search = is_backend_web_search(tc.meta.as_ref(), &raw_input);
    ToolCallEvent {
        tool_call_id: tc.tool_call_id.0.to_string(),
        title: tc.title.clone(),
        tool_kind,
        status: Some(tc.status),
        tool_name,
        raw_input,
        content: json_array_or_empty(&tc.content),
        locations: json_array_or_empty(&tc.locations),
        backend_web_search,
    }
}

fn tool_call_update_event(tcu: &proto::ToolCallUpdate) -> ToolCallUpdateEvent {
    ToolCallUpdateEvent {
        tool_call_id: tcu.tool_call_id.0.to_string(),
        status: tcu.fields.status,
        content: tcu
            .fields
            .content
            .as_ref()
            .map_or_else(|| json!([]), json_array_or_empty),
        raw_output: tcu.fields.raw_output.clone().unwrap_or(Value::Null),
        locations: tcu
            .fields
            .locations
            .as_ref()
            .map_or_else(|| json!([]), json_array_or_empty),
    }
}

fn tools_from_meta(meta: Option<&proto::Meta>) -> Vec<String> {
    meta.and_then(|m| m.get("tools"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn command_names(commands: &[proto::AvailableCommand]) -> Vec<String> {
    commands.iter().map(|c| c.name.clone()).collect()
}

/// Skill names from the `AvailableCommandsUpdate`: commands stamped with `_meta.scope` + `_meta.path`.
pub(crate) fn skill_names(commands: &[proto::AvailableCommand]) -> Vec<String> {
    commands
        .iter()
        .filter(|c| {
            c.meta
                .as_ref()
                .is_some_and(|m| m.get("scope").is_some() && m.get("path").is_some())
        })
        .map(|c| c.name.clone())
        .collect()
}

/// Map an ACP `SessionUpdate` to a [`StreamEvent`], or `None` for unsurfaced variants.
pub(crate) fn map_session_update(update: &proto::SessionUpdate) -> Option<StreamEvent> {
    Some(match update {
        proto::SessionUpdate::ToolCall(tc) => StreamEvent::ToolCall(tool_call_event(tc)),
        proto::SessionUpdate::ToolCallUpdate(tcu) => {
            StreamEvent::ToolCallUpdate(tool_call_update_event(tcu))
        }
        proto::SessionUpdate::Plan(p) => StreamEvent::Plan(json_array_or_empty(&p.entries)),
        proto::SessionUpdate::AvailableCommandsUpdate(u) => StreamEvent::AvailableCommands {
            tools: tools_from_meta(u.meta.as_ref()),
            commands: command_names(&u.available_commands),
            skills: skill_names(&u.available_commands),
        },
        _ => return None,
    })
}

/// Session facts known once the ACP session is open.
pub(crate) struct SessionContext {
    pub session_id: String,
    pub model: Option<String>,
    pub cwd: String,
    pub permission_mode: Option<String>,
    pub mcp_servers: Vec<McpServer>,
    pub include_partial_messages: bool,
    /// True when the session authenticated with an API key (vs OAuth).
    pub api_key_auth: bool,
    /// The current model's total context window in tokens, when known.
    pub context_window: Option<u64>,
}

pub(crate) struct TurnEnd<'a> {
    pub stop_reason: &'a str,
    pub session_id: &'a str,
    pub request_id: &'a str,
    pub usage: Option<&'a Value>,
    pub structured_output: Option<Result<Value, String>>,
    pub result_text: &'a str,
    /// Total wall-clock for the run (`duration_ms` on the Messages `result`).
    pub duration_ms: u64,
}

/// One entry of the Messages API `system`/`init` `mcp_servers` list.
#[derive(Clone, Serialize)]
pub(crate) struct McpServer {
    pub name: String,
    pub status: String,
}

/// Folds the agent event stream into NDJSON lines for one wire format.
pub(crate) trait Reducer {
    fn begin(&mut self, _ctx: SessionContext) -> Vec<Value> {
        Vec::new()
    }

    fn reduce(&mut self, event: StreamEvent) -> Vec<Value>;

    fn max_turns(&mut self) -> Vec<Value> {
        Vec::new()
    }

    fn finish(&mut self, end: &TurnEnd<'_>) -> Vec<Value>;

    /// Terminal error line(s). `stop_reason` is a Messages-only override (e.g. `max_tokens`); `None` keeps the fallback.
    fn error(
        &mut self,
        message: &str,
        usage: Option<&Value>,
        duration_ms: u64,
        stop_reason: Option<&str>,
    ) -> Vec<Value>;
}

/// The reducer for `format`, or `None` for `plain`/`json` (rendered directly).
pub(crate) fn reducer_for(format: OutputFormat) -> Option<Box<dyn Reducer>> {
    match format {
        OutputFormat::StreamingJson => Some(Box::new(AcpReducer)),
        OutputFormat::StreamingMessagesJson => Some(Box::new(MessagesReducer::new())),
        OutputFormat::Plain | OutputFormat::Json => None,
    }
}
