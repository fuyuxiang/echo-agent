//! The streaming-json `AcpReducer` native-shape mapping.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn acp_reducer_maps_agent_message_to_text() {
    let mut r = AcpReducer;
    assert_eq!(
        r.reduce(StreamEvent::AgentMessage("hi".into()))[0],
        json!({"type": "text", "data": "hi"})
    );
}

#[test]
fn acp_reducer_maps_tool_call_to_native_shape() {
    let mut r = AcpReducer;
    assert_eq!(
        r.reduce(StreamEvent::ToolCall(tool_call_ev()))[0],
        json!({
            "type": "tool_call",
            "toolCallId": "t1",
            "title": "Bash",
            "kind": "execute",
            "status": "in_progress",
            "toolName": "bash",
            "rawInput": {"command": "ls"},
            "content": [],
            "locations": [],
        })
    );
}

#[test]
fn acp_reducer_maps_tool_call_update_to_native_shape() {
    let mut r = AcpReducer;
    assert_eq!(
        r.reduce(StreamEvent::ToolCallUpdate(tool_update(
            "completed",
            json!({"ok": true}),
        )))[0],
        json!({
            "type": "tool_call_update",
            "toolCallId": "t1",
            "status": "completed",
            "content": [],
            "rawOutput": {"ok": true},
            "locations": [],
        })
    );
}

#[test]
fn acp_response_completed_emits_usage_line() {
    let mut r = AcpReducer;
    let out = r.reduce(StreamEvent::ResponseCompleted {
        message_id: Some("msg_1".into()),
        stop_reason: Some("tool_use".into()),
        usage: Some(ResponseUsage {
            input_tokens: 5,
            output_tokens: 2,
            ..Default::default()
        }),
        signature: Some("sig".into()),
        stop_sequence: None,
    });
    assert_eq!(out[0]["type"], "usage");
    assert_eq!(out[0]["messageId"], "msg_1");
    assert_eq!(out[0]["stopReason"], "tool_use");
    assert_eq!(out[0]["usage"]["input_tokens"], 5);
    assert_eq!(out[0]["signature"], "sig");
}

#[test]
fn acp_finish_emits_end_line_with_usage_and_structured_output() {
    let mut r = AcpReducer;
    let aggregate = json!({
        "inputTokens": 5,
        "outputTokens": 2,
        "totalTokens": 7,
        "numTurns": 1,
    });
    let out = r.finish(&TurnEnd {
        stop_reason: "end_turn",
        session_id: "sess-1",
        request_id: "req-1",
        usage: Some(&aggregate),
        structured_output: Some(Ok(json!({"name": "alice"}))),
        result_text: "",
        duration_ms: 0,
    });
    let end = out.last().unwrap();
    assert_eq!(end["type"], "end");
    assert_eq!(end["stopReason"], "end_turn");
    assert_eq!(end["sessionId"], "sess-1");
    assert_eq!(end["requestId"], "req-1");
    assert_eq!(end["structuredOutput"]["name"], "alice");
    assert!(end["usage"].is_object());
}
