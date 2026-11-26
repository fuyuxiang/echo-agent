//! Envelope-shape tests for the JSON-RPC 2.0 wrappers.

use echo_agent_tool_protocol::{
    FrameSeq, JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    JsonRpcVersion, RequestId, ResponseOutcome, SessionId,
};
use serde_json::{Value, json};

fn session() -> SessionId {
    SessionId::new("sess_abc").unwrap()
}

#[test]
fn request_with_no_session_id_omits_envelope_field() {
    let req: JsonRpcRequest<Value> = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::new_string("req-1"),
        session_id: None,
        method: "tool.call".to_owned(),
        params: json!({}),
    };
    let v = serde_json::to_value(&req).unwrap();
    let obj = v.as_object().unwrap();
    assert!(
        !obj.contains_key("session_id"),
        "session_id=None must be omitted: {v}"
    );
    assert_eq!(v["jsonrpc"], json!("2.0"));
    assert_eq!(v["id"], json!("req-1"));
    assert_eq!(v["method"], json!("tool.call"));
}

#[test]
fn request_with_session_id_includes_envelope_field() {
    let req: JsonRpcRequest<Value> = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::new_string("req-2"),
        session_id: Some(session()),
        method: "tool.call".to_owned(),
        params: json!({"x": 1}),
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["session_id"], json!("sess_abc"));
    assert_eq!(v["params"]["x"], json!(1));
    let parsed: JsonRpcRequest<Value> = serde_json::from_value(v).unwrap();
    assert_eq!(parsed.id, JsonRpcId::new_string("req-2"));
    assert_eq!(
        parsed.session_id.as_ref().map(|s| s.as_str()),
        Some("sess_abc")
    );
}

#[test]
fn notification_with_no_seq_omits_envelope_field() {
    let n: JsonRpcNotification<Value> = JsonRpcNotification {
        jsonrpc: JsonRpcVersion,
        session_id: None,
        seq: None,
        method: "tool.notification".to_owned(),
        params: json!({}),
    };
    let v = serde_json::to_value(&n).unwrap();
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("seq"));
    assert!(!obj.contains_key("session_id"));
    assert!(!obj.contains_key("id"));
}

#[test]
fn notification_with_seq_includes_envelope_field() {
    let n: JsonRpcNotification<Value> = JsonRpcNotification {
        jsonrpc: JsonRpcVersion,
        session_id: Some(session()),
        seq: Some(FrameSeq::new(42)),
        method: "tool.notification".to_owned(),
        params: json!({}),
    };
    let v = serde_json::to_value(&n).unwrap();
    assert_eq!(v["seq"], json!(42));
}

#[test]
fn response_ok_serialises_with_result_only() {
    let resp: JsonRpcResponse<Value> =
        JsonRpcResponse::ok(JsonRpcId::new_string("r"), json!({"y": 2}));
    let v = serde_json::to_value(&resp).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(v["jsonrpc"], json!("2.0"));
    assert_eq!(v["id"], json!("r"));
    assert_eq!(v["result"], json!({"y": 2}));
    assert!(!obj.contains_key("error"), "ok must omit `error`: {v}");
}

#[test]
fn response_err_serialises_with_error_only() {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse::err(
        JsonRpcId::new_string("r"),
        JsonRpcError {
            code: -32011,
            message: "tool not found".to_owned(),
            data: Some(json!({"code": "tool_not_found"})),
        },
    );
    let v = serde_json::to_value(&resp).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(v["error"]["code"], json!(-32011));
    assert_eq!(v["error"]["data"]["code"], json!("tool_not_found"));
    assert!(!obj.contains_key("result"), "err must omit `result`: {v}");
}

#[test]
fn response_round_trips_with_session_envelope() {
    let resp: JsonRpcResponse<Value> =
        JsonRpcResponse::ok(JsonRpcId::Number(7), json!({})).with_session(session());
    let v = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["session_id"], json!("sess_abc"));
    assert_eq!(v["id"], json!(7));
    let parsed: JsonRpcResponse<Value> = serde_json::from_value(v).unwrap();
    assert_eq!(parsed.id, JsonRpcId::Number(7));
    match parsed.outcome {
        ResponseOutcome::Result(_) => {}
        ResponseOutcome::Error(e) => panic!("expected Result, got Error({e:?})"),
    }
}

#[test]
fn response_with_both_result_and_error_fails_to_deserialize() {
    let bad = json!({
        "jsonrpc": "2.0",
        "id": "r",
        "result": {"x": 1},
        "error": {"code": -32000, "message": "no"},
    });
    let err = serde_json::from_value::<JsonRpcResponse<Value>>(bad).unwrap_err();
    assert!(
        err.to_string().contains("XOR"),
        "expected XOR-violation message, got: {err}"
    );
}

#[test]
fn response_with_neither_result_nor_error_fails_to_deserialize() {
    let bad = json!({"jsonrpc": "2.0", "id": "r"});
    let err = serde_json::from_value::<JsonRpcResponse<Value>>(bad).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("`result` or `error`"),
        "expected exactly-one message, got: {msg}"
    );
}

#[test]
fn jsonrpc_error_round_trips_with_and_without_data() {
    let e_no_data = JsonRpcError {
        code: -32603,
        message: "internal".to_owned(),
        data: None,
    };
    let v = serde_json::to_value(&e_no_data).unwrap();
    assert!(
        !v.as_object().unwrap().contains_key("data"),
        "data=None must be omitted: {v}"
    );
    let back: JsonRpcError = serde_json::from_value(v).unwrap();
    assert_eq!(back, e_no_data);

    let e_with_data = JsonRpcError {
        code: -32011,
        message: "tool not found".to_owned(),
        data: Some(json!({"code": "tool_not_found", "tool_id": "echo"})),
    };
    let v = serde_json::to_value(&e_with_data).unwrap();
    assert_eq!(v["data"]["tool_id"], json!("echo"));
    let back: JsonRpcError = serde_json::from_value(v).unwrap();
    assert_eq!(back, e_with_data);
}

#[test]
fn jsonrpc_id_accepts_string_and_number_on_request() {
    let v_str = json!({
        "jsonrpc": "2.0",
        "id": "req-9c4f",
        "method": "tool.call",
        "params": {},
    });
    let req: JsonRpcRequest<Value> = serde_json::from_value(v_str).unwrap();
    assert_eq!(req.id, JsonRpcId::new_string("req-9c4f"));

    let v_num = json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "tool.call",
        "params": {},
    });
    let req: JsonRpcRequest<Value> = serde_json::from_value(v_num).unwrap();
    assert_eq!(req.id, JsonRpcId::Number(99));
}

#[test]
fn jsonrpc_id_round_trips_to_request_id_correlator() {
    let original = RequestId::new("req-42").unwrap();
    let envelope_id = JsonRpcId::from_request_id(&original);
    assert_eq!(envelope_id.as_request_id().unwrap(), original);

    // Numeric ids are stringified.
    let nid = JsonRpcId::Number(7);
    assert_eq!(nid.as_request_id().unwrap().as_str(), "7");
}

#[test]
fn full_call_envelope_serialises_to_expected_shape() {
    use echo_agent_tool_protocol::{ToolCallId, ToolCallParams, ToolId};
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::new_string("req-9c4f"),
        session_id: Some(session()),
        method: "tool.call".to_owned(),
        params: ToolCallParams {
            tool_call_id: ToolCallId::new("call_xyz").unwrap(),
            tool_id: ToolId::new("EchoAgentBuild:read_file").unwrap(),
            arguments: json!({"path": "/etc/hosts"}),
            deadline_ms: None,
            behavior_version: None,
            cwd: None,
            trace_context: None,
        },
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["jsonrpc"], json!("2.0"));
    assert_eq!(v["id"], json!("req-9c4f"));
    assert_eq!(v["session_id"], json!("sess_abc"));
    assert_eq!(v["method"], json!("tool.call"));
    assert_eq!(v["params"]["tool_id"], json!("EchoAgentBuild:read_file"));
    assert_eq!(v["params"]["tool_call_id"], json!("call_xyz"));
}

/// The envelope-level `session_id` and an inner `params.session_id` (e.g.
/// on `ToolsListParams`) are independent keys in the wire JSON tree.
/// This test pins that invariant so a refactor that accidentally
/// collapses the two layers (e.g. via `#[serde(flatten)]`) fails loudly.
#[test]
fn envelope_session_id_and_inner_params_session_id_are_distinct_layers() {
    use echo_agent_tool_protocol::{ToolDefinitionMode, ToolsListParams};
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::new_string("req-mix"),
        session_id: Some(session()),
        method: "tools.list".to_owned(),
        params: ToolsListParams {
            session_id: session(),
            mode: ToolDefinitionMode::Full,
        },
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["session_id"], json!("sess_abc"));
    assert_eq!(v["params"]["session_id"], json!("sess_abc"));
    let top_keys: std::collections::BTreeSet<&str> =
        v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        top_keys,
        ["id", "jsonrpc", "method", "params", "session_id"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
    );
}
