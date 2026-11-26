//! Decode-side coverage for `ToolErrorWire::Internal`'s optional `detail`:
//! a populated detail must become the reconstructed `ToolError`'s message,
//! and its absence (frames from older peers) must fall back to the historic
//! constant.

use echo_agent_computer_hub_core::{error_from_envelope, tool_error_from_wire};
use echo_agent_tool_protocol::{JsonRpcError, RequestId, ToolErrorWire};
use echo_agent_tool_runtime::ToolErrorKind;
use serde_json::json;

#[test]
fn internal_with_detail_reconstructs_the_wire_detail() {
    let err = tool_error_from_wire(ToolErrorWire::Internal {
        request_id: None,
        detail: Some("cross-instance tool.call timed out".to_owned()),
    });
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert_eq!(err.detail, "cross-instance tool.call timed out");
    // The `internal_error` code survives so callers can still classify it.
    assert_eq!(
        err.details
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(|v| v.as_str()),
        Some("internal_error"),
    );
}

#[test]
fn internal_without_detail_falls_back_to_the_historic_constant() {
    let err = tool_error_from_wire(ToolErrorWire::Internal {
        request_id: None,
        detail: None,
    });
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert_eq!(err.detail, "internal router error");
}

#[test]
fn internal_with_request_id_keeps_both_code_and_request_id() {
    let err = tool_error_from_wire(ToolErrorWire::Internal {
        request_id: Some(RequestId::new("req-7").unwrap()),
        detail: Some("relay publish failed".to_owned()),
    });
    assert_eq!(err.detail, "relay publish failed");
    let details = err.details.expect("details present");
    assert_eq!(details["code"], json!("internal_error"));
    assert_eq!(details["request_id"], json!("req-7"));
}

#[test]
fn envelope_with_internal_data_prefers_data_detail_over_message() {
    // The hub's `-32000 "internal error"` envelope keeps its constant message;
    // the harness must read the cause from `error.data`, not the message.
    let err = error_from_envelope(JsonRpcError {
        code: -32000,
        message: "internal error".to_owned(),
        data: Some(json!({
            "code": "internal_error",
            "detail": "cross-instance call cancelled",
        })),
    });
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert_eq!(err.detail, "cross-instance call cancelled");
}
