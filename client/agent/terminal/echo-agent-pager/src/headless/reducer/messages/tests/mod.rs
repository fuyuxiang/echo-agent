//! Reducer test suite. A child module of `messages` so it can reach
//! `MessagesReducer`'s private state directly (and the coordinator's re-exported
//! `wire`/`state` items via `use super::*`), while pulling the shared
//! transport/`acp` reducer items in from the crate root.

use super::usage::messages_model_usage;
use super::wire::ModelUsage;
use super::*;
use crate::headless::reducer::acp::AcpReducer;
use crate::headless::reducer::{McpServer, skill_names, tool_call_event};
use echo_agent_runtime::extensions::notification::ResponseUsage;
use serde::Serialize;
use serde_json::{Value, json};

fn tool_call_ev() -> ToolCallEvent {
    ToolCallEvent {
        tool_call_id: "t1".into(),
        title: "Bash".into(),
        tool_kind: Some("execute".into()),
        status: Some(acp::ToolCallStatus::InProgress),
        tool_name: "bash".into(),
        raw_input: json!({"command": "ls"}),
        content: json!([]),
        locations: json!([]),
        backend_web_search: false,
    }
}

/// A backend `web_search` `ToolCall`, as `tool_call_event` classifies it from
/// the shell's `_meta.backend == true` + `raw_input.variant == "WebSearch"`.
fn web_search_call(id: &str) -> ToolCallEvent {
    ToolCallEvent {
        tool_call_id: id.into(),
        title: "Web search:".into(),
        tool_kind: Some("search".into()),
        status: Some(acp::ToolCallStatus::InProgress),
        tool_name: "web_search".into(),
        raw_input: json!({"variant": "WebSearch", "backend": true}),
        content: json!([]),
        locations: json!([]),
        backend_web_search: true,
    }
}

/// A terminal backend `web_search` `ToolCallUpdate` carrying EchoAgent's nested
/// `WebSearchCall` `raw_output` (`action.query` + `action.sources[].url`).
fn web_search_done(id: &str) -> ToolCallUpdateEvent {
    ToolCallUpdateEvent {
        tool_call_id: id.into(),
        status: Some(acp::ToolCallStatus::Completed),
        content: json!([]),
        raw_output: json!({
            "id": id,
            "type": "web_search_call",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "rust async runtime",
                "sources": [
                    {"type": "url", "url": "https://tokio.rs", "title": "Tokio"},
                    {"type": "url", "url": "https://async.rs"},
                ],
            },
        }),
        locations: json!([]),
    }
}

fn tool_update(status: &str, raw_output: Value) -> ToolCallUpdateEvent {
    ToolCallUpdateEvent {
        tool_call_id: "t1".into(),
        status: serde_json::from_value(Value::String(status.into())).ok(),
        content: json!([]),
        raw_output,
        locations: json!([]),
    }
}

fn messages(partials: bool) -> MessagesReducer {
    let mut r = MessagesReducer::new();
    r.begin(SessionContext {
        session_id: "sess-1".into(),
        model: Some("echo-agent-4".into()),
        cwd: "/repo".into(),
        permission_mode: Some("bypassPermissions".into()),
        mcp_servers: vec![McpServer {
            name: "linear".into(),
            status: "connected".into(),
        }],
        include_partial_messages: partials,
        api_key_auth: true,
        context_window: Some(256_000),
    });
    r
}

/// A skill command carries `_meta.scope` + `_meta.path`; a workflow carries
/// `workflowPath`/`workflowSource`; a builtin carries no `_meta`. Only the
/// skill is projected into `init.skills`.
fn skill_command(name: &str) -> acp::AvailableCommand {
    let meta = serde_json::json!({"scope": "user", "path": "/skills/foo.md"})
        .as_object()
        .cloned();
    acp::AvailableCommand::new(name.to_string(), "a skill".to_string()).meta(meta)
}

fn workflow_command(name: &str) -> acp::AvailableCommand {
    let meta = serde_json::json!({"workflowSource": "user", "workflowPath": "/wf.md"})
        .as_object()
        .cloned();
    acp::AvailableCommand::new(name.to_string(), "a workflow".to_string()).meta(meta)
}

fn builtin_command(name: &str) -> acp::AvailableCommand {
    acp::AvailableCommand::new(name.to_string(), "a builtin".to_string())
}

fn stream_delta(out: &[Value]) -> &Value {
    out.iter()
        .find(|m| m["type"] == "stream_event" && m["event"]["type"] == "content_block_delta")
        .expect("a content_block_delta stream_event")
}

/// A `Failed` terminal backend `web_search` update carrying no results.
fn web_search_failed(id: &str) -> ToolCallUpdateEvent {
    ToolCallUpdateEvent {
        tool_call_id: id.into(),
        status: Some(acp::ToolCallStatus::Failed),
        content: json!([]),
        raw_output: json!({"id": id, "type": "web_search_call", "status": "failed"}),
        locations: json!([]),
    }
}

/// A completed backend `WebSearch` update for a NON-search action (e.g.
/// open_page): the `raw_output` carries no `action.query`/`action.sources`.
fn web_search_non_search(id: &str) -> ToolCallUpdateEvent {
    ToolCallUpdateEvent {
        tool_call_id: id.into(),
        status: Some(acp::ToolCallStatus::Completed),
        content: json!([]),
        raw_output: json!({
            "id": id,
            "type": "web_search_call",
            "status": "completed",
            "action": {"type": "open_page", "url": "https://example.com"},
        }),
        locations: json!([]),
    }
}

/// A small `TurnEnd` for a clean end-of-turn flush at `sess-1`.
fn end_turn() -> TurnEnd<'static> {
    TurnEnd {
        stop_reason: "end_turn",
        session_id: "sess-1",
        request_id: "req-1",
        usage: None,
        structured_output: None,
        result_text: "",
        duration_ms: 0,
    }
}

/// A `ResponseStarted` with the given id/model and input tokens; cache buckets zero.
fn response_started(id: &str, model: Option<&str>, input_tokens: u64) -> StreamEvent {
    StreamEvent::ResponseStarted {
        message_id: Some(id.into()),
        model: model.map(str::to_string),
        input_tokens,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    }
}

/// A `ResponseCompleted` carrying only an id and stop reason (no usage/signature/stop sequence).
fn response_completed(id: &str, stop_reason: &str) -> StreamEvent {
    StreamEvent::ResponseCompleted {
        message_id: Some(id.into()),
        stop_reason: Some(stop_reason.into()),
        usage: None,
        signature: None,
        stop_sequence: None,
    }
}

/// A `TurnEnd` at `sess-1`/`req-1` with no usage/structured output and zero duration.
fn turn_end(stop_reason: &'static str, result_text: &'static str) -> TurnEnd<'static> {
    TurnEnd {
        stop_reason,
        session_id: "sess-1",
        request_id: "req-1",
        usage: None,
        structured_output: None,
        result_text,
        duration_ms: 0,
    }
}

mod acp_reducer;
mod content;
mod init;
mod partial;
mod result_usage;
mod tool_calls;
mod web_search;
