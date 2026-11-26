//! Serde round-trip coverage for every public type and the wire-rename
//! assertions for each variant.
//!
//! Companion files: `identifier_validation.rs` covers id-newtype
//! constructor and validator behaviour; `tool_id_derivation.rs` covers
//! `ToolDescriptionWithSchema::derive_tool_id`.

use std::collections::HashMap;

use echo_agent_tool_protocol::{
    AttachRoute, ConnectionId, ConnectionKind, ERROR_CODES, FrameSeq, HelloAckMsg, HelloMsg,
    HookEvent, HookFrame, HookKind, IMAGE_CAPABILITIES_V1, JsonRpcId, JsonRpcVersion,
    KNOWN_NOTIFICATION_KINDS, LastSeq, McpBlock, Method, NotificationFilter, NotificationSchemas,
    PingFrame, PongFrame, RegistrationOutcome, RegistryError, RequestId, ServerBindAck,
    ServerBindOutcome, ServerId, SessionAttachServerParams, SessionAttachServerResult,
    SessionBindResult, SessionBindServerParams, SessionBindServerResult, SessionCloseParams,
    SessionEvent, SessionId, SessionOpenParams, SessionPhase, SessionUnbindServerParams,
    StreamingSpec, SubscribeAck, SubscribeNotificationsParams, SubscribeOutcome, ToolCallId,
    ToolCallOutcome, ToolCallParams, ToolCallProgressFrame, ToolCallResult, ToolCapabilities,
    ToolDefinitionMode, ToolDescriptionWithSchema, ToolErrorWire, ToolId, ToolNotificationFrame,
    ToolOutputWire, ToolRegistration, ToolScope, ToolSearchResult, ToolServerRegistration,
    ToolsChanged, ToolsListParams, ToolsListResult, ToolsSearchParams, ToolsSearchResultBody,
    TransportKind, UnsubscribeAck, UnsubscribeNotificationsParams, UnsubscribeOutcome, UserId,
    WireCustomNotification, WireToolNotification, error_codes,
};
use echo_agent_tool_types::ToolDescription;
use serde_json::{Value, json};

fn roundtrip<T>(value: &T) -> Value
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).expect("serialize");
    let parsed: T = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(&parsed, value, "round-trip mismatch");
    json
}

fn session() -> SessionId {
    SessionId::new("sess_abc").unwrap()
}

fn user() -> UserId {
    UserId::new("user_123").unwrap()
}

fn tool() -> ToolId {
    ToolId::new("EchoAgentBuild:read_file").unwrap()
}

fn server() -> ServerId {
    ServerId::new("srv-uuidv7").unwrap()
}

fn call_id() -> ToolCallId {
    ToolCallId::new("call_alpha").unwrap()
}

#[test]
fn id_types_round_trip_as_bare_strings() {
    assert_eq!(roundtrip(&session()), json!("sess_abc"));
    assert_eq!(roundtrip(&user()), json!("user_123"));
    assert_eq!(roundtrip(&tool()), json!("EchoAgentBuild:read_file"));
    assert_eq!(roundtrip(&server()), json!("srv-uuidv7"));
    assert_eq!(
        roundtrip(&ConnectionId::new("conn_42").unwrap()),
        json!("conn_42")
    );
    assert_eq!(
        roundtrip(&RequestId::new("req-9c4f").unwrap()),
        json!("req-9c4f")
    );
    let call_id = ToolCallId::new("call_1").unwrap();
    assert_eq!(roundtrip(&call_id), json!("call_1"));
}

#[test]
fn frame_seq_round_trips_as_number() {
    assert_eq!(roundtrip(&FrameSeq::default()), json!(0));
    assert_eq!(roundtrip(&FrameSeq::new(42)), json!(42));
    assert_eq!(roundtrip(&FrameSeq::new(u64::MAX)), json!(u64::MAX));
}

#[test]
fn id_deserialise_rejects_invalid_values() {
    let err = serde_json::from_value::<SessionId>(json!("")).unwrap_err();
    assert!(err.to_string().contains("must not be empty"), "{err}");

    let err = serde_json::from_value::<ServerId>(json!("auto:my-server")).unwrap_err();
    assert!(err.to_string().contains("reserved prefix"), "{err}");

    let err = serde_json::from_value::<ToolId>(json!("foo:bar:baz")).unwrap_err();
    assert!(err.to_string().contains("invalid format"), "{err}");
}

#[test]
fn tool_capabilities_default_round_trips_and_omits_optionals() {
    let caps = ToolCapabilities::default();
    let json = roundtrip(&caps);
    let obj = json.as_object().unwrap();
    // Bool fields default to `false` and are present so wire consumers can
    // rely on their presence; optional fields are skipped.
    assert_eq!(obj["supports_cancel"], json!(false));
    assert_eq!(obj["is_read_only"], json!(false));
    assert!(!obj.contains_key("streaming"));
    assert!(!obj.contains_key("max_concurrency"));
    assert!(!obj.contains_key("hooks"));
    assert!(!obj.contains_key("behavior_version"));
    assert!(!obj.contains_key("max_frame_bytes"));
    assert!(!obj.contains_key("timeout_ms"));
    assert!(!obj.contains_key("tool_scope"));
}

#[test]
fn tool_capabilities_with_all_fields_round_trips() {
    let caps = ToolCapabilities {
        streaming: Some(StreamingSpec {
            subkind: "bash_output_chunk".to_owned(),
            max_delta_bytes: Some(16 * 1024),
        }),
        supports_cancel: true,
        max_concurrency: Some(8),
        is_read_only: true,
        hooks: vec![HookKind::OnToolCallStart, HookKind::OnCancel],
        behavior_version: Some("v1".to_owned()),
        max_frame_bytes: Some(1_048_576),
        timeout_ms: Some(30_000),
        tool_scope: Some(ToolScope::Write),
    };
    roundtrip(&caps);
}

#[test]
fn hook_kind_serialises_snake_case() {
    let cases = [
        (HookKind::OnSessionOpen, "on_session_open"),
        (HookKind::OnSessionClose, "on_session_close"),
        (HookKind::OnToolCallStart, "on_tool_call_start"),
        (HookKind::OnToolCallResult, "on_tool_call_result"),
        (HookKind::OnCancel, "on_cancel"),
        (HookKind::OnNotification, "on_notification"),
    ];
    for (kind, expected) in cases {
        let v = roundtrip(&kind);
        assert_eq!(v, json!(expected));
    }
}

#[test]
fn notification_schemas_round_trip_with_and_without_entries() {
    roundtrip(&NotificationSchemas::default());
    let mut outbound = HashMap::new();
    outbound.insert("MyKind".to_owned(), json!({"type": "object"}));
    let mut inbound = HashMap::new();
    inbound.insert("OtherKind".to_owned(), json!({"type": "string"}));
    roundtrip(&NotificationSchemas { outbound, inbound });
}

#[test]
fn hook_event_variants_round_trip() {
    assert_eq!(roundtrip(&HookEvent::Cancel), json!({"type": "Cancel"}));
    assert_eq!(roundtrip(&HookEvent::Pause), json!({"type": "Pause"}));
    assert_eq!(roundtrip(&HookEvent::Resume), json!({"type": "Resume"}));
    assert_eq!(
        roundtrip(&HookEvent::SessionEnded),
        json!({"type": "SessionEnded"})
    );
    assert_eq!(
        roundtrip(&HookEvent::Custom {
            kind: "my_tool.progress".to_owned(),
            payload: json!({"pct": 42}),
        }),
        json!({
            "type": "Custom",
            "kind": "my_tool.progress",
            "payload": {"pct": 42},
        })
    );
}

#[test]
fn connection_kind_and_definition_mode_full_serialise() {
    assert_eq!(roundtrip(&ConnectionKind::Harness), json!("harness"));
    assert_eq!(roundtrip(&ConnectionKind::ToolServer), json!("tool_server"));
    assert_eq!(
        roundtrip(&ToolDefinitionMode::Full),
        json!({"mode": "full"})
    );
}

#[test]
fn handshake_messages_round_trip() {
    let hello = HelloMsg {
        protocol_version: "1".to_owned(),
        kind: ConnectionKind::ToolServer,
        server_id: None,
        description: None,
        metadata: None,
    };
    let json = roundtrip(&hello);
    assert!(
        !json.as_object().unwrap().contains_key("session_id"),
        "hello must not carry session_id"
    );
    assert!(
        !json.as_object().unwrap().contains_key("session_ids"),
        "hello must not carry session_ids"
    );
    assert!(
        !json.as_object().unwrap().contains_key("user_id"),
        "hello must not carry user_id"
    );
    roundtrip(&hello);

    let ack = HelloAckMsg {
        connection_id: ConnectionId::new("conn_1").unwrap(),
        user_id: user(),
        computer_hub_version: "0.1.0".to_owned(),
        supported_protocol_versions: vec!["1".to_owned()],
        capabilities: vec![],
    };
    let ack_json = roundtrip(&ack);
    assert!(
        ack_json.as_object().unwrap().contains_key("user_id"),
        "hello_ack must carry user_id"
    );
    assert!(
        !ack_json.as_object().unwrap().contains_key("capabilities"),
        "empty capabilities must be omitted for old-peer compatibility"
    );

    let mut legacy = ack_json;
    legacy.as_object_mut().unwrap().remove("capabilities");
    let parsed: HelloAckMsg =
        serde_json::from_value(legacy).expect("legacy hello_ack without capabilities parses");
    assert!(parsed.capabilities.is_empty());
}

fn sample_description(name: &str, namespace: Option<&str>) -> ToolDescription {
    let mut d = ToolDescription::new(name, "test description");
    if let Some(ns) = namespace {
        d = d.with_namespace(ns);
    }
    d
}

#[test]
fn transport_kind_round_trips_snake_case() {
    assert_eq!(roundtrip(&TransportKind::Local), json!("local"));
    assert_eq!(roundtrip(&TransportKind::Remote), json!("remote"));
}

#[test]
fn tool_registration_round_trips_with_and_without_server_id() {
    let base = ToolRegistration {
        tool_id: tool(),
        sessions: Some(vec![session()]),
        user_id: user(),
        server_id: None,
        description: sample_description("read_file", Some("EchoAgentBuild")),
        input_schema: Some(json!({"type": "object"})),
        capabilities: None,
        notification_schemas: None,
        transport_kind: TransportKind::Remote,
        if_match_generation: None,
        metadata: None,
    };

    let json_no_server = roundtrip(&base);
    let obj = json_no_server.as_object().unwrap();
    assert!(
        !obj.contains_key("server_id"),
        "server_id=None must be omitted from JSON: {json_no_server}"
    );
    assert!(
        !obj.contains_key("if_match_generation"),
        "if_match_generation=None must be omitted: {json_no_server}"
    );
    assert!(
        !obj.contains_key("capabilities"),
        "capabilities=None must be omitted from JSON: {json_no_server}"
    );
    assert!(
        !obj.contains_key("notification_schemas"),
        "notification_schemas=None must be omitted from JSON: {json_no_server}"
    );

    let with_server = ToolRegistration {
        server_id: Some(server()),
        if_match_generation: Some(7),
        metadata: None,
        capabilities: Some(ToolCapabilities {
            is_read_only: true,
            ..Default::default()
        }),
        notification_schemas: Some(NotificationSchemas::default()),
        ..base
    };
    let json_with_server = roundtrip(&with_server);
    assert_eq!(json_with_server["server_id"], json!("srv-uuidv7"));
    assert_eq!(json_with_server["if_match_generation"], json!(7));
    assert_eq!(
        json_with_server["capabilities"]["is_read_only"],
        json!(true)
    );
    // None vs Some(default) is distinguishable on the wire.
    assert!(json_with_server["notification_schemas"].is_object());
}

#[test]
fn tool_server_registration_round_trips_empty_and_populated() {
    let empty = ToolServerRegistration {
        server_id: server(),
        sessions: None,
        user_id: user(),
        title: None,
        description: String::new(),
        tools: Vec::new(),
        hooks: Vec::new(),
        if_match_generation: None,
        metadata: None,
    };
    let json = roundtrip(&empty);
    let obj = json.as_object().unwrap();
    assert!(!obj.contains_key("title"));
    assert!(!obj.contains_key("hooks"));
    assert!(!obj.contains_key("if_match_generation"));
    assert!(
        !obj.contains_key("sessions"),
        "sessions=None means \"no change\" and must be omitted from JSON: {json}"
    );
    assert!(
        !obj.contains_key("description"),
        "empty description must be omitted from JSON: {json}"
    );

    let populated = ToolServerRegistration {
        server_id: server(),
        sessions: Some(vec![session()]),
        user_id: user(),
        title: Some("GitHub Tools".to_owned()),
        description: "Read repos and issues".to_owned(),
        tools: vec![ToolDescriptionWithSchema {
            description: sample_description("list_repos", Some("github")),
            input_schema: Some(json!({"type": "object"})),
            capabilities: Some(ToolCapabilities {
                is_read_only: true,
                ..Default::default()
            }),
            notification_schemas: Some(NotificationSchemas::default()),
        }],
        hooks: vec![HookKind::OnSessionClose],
        if_match_generation: Some(3),
        metadata: None,
    };
    roundtrip(&populated);
}

#[test]
fn registration_outcome_variants_round_trip() {
    let cases = [
        (
            RegistrationOutcome::Registered {
                tool_id: tool(),
                generation: 1,
            },
            "registered",
        ),
        (
            RegistrationOutcome::Updated {
                tool_id: tool(),
                generation: 2,
            },
            "updated",
        ),
        (
            RegistrationOutcome::Shadowed {
                tool_id: tool(),
                reason: "local_priority".to_owned(),
            },
            "shadowed",
        ),
        (
            RegistrationOutcome::Rejected {
                tool_id: tool(),
                code: "invalid_description".to_owned(),
                message: "missing required field".to_owned(),
            },
            "rejected",
        ),
    ];
    for (outcome, expected_tag) in cases {
        let json = roundtrip(&outcome);
        assert_eq!(json["outcome"], json!(expected_tag));
        assert_eq!(json["tool_id"], json!("EchoAgentBuild:read_file"));
    }
}

#[test]
fn registry_error_already_registered_uses_renamed_code() {
    let err = RegistryError::AlreadyRegistered { tool_id: tool() };
    let json = roundtrip(&err);
    // Wire `code` MUST be the literal `tool_already_registered`, not
    // `already_registered` (which is what `rename_all = "snake_case"`
    // would produce without the per-variant rename).
    assert_eq!(
        json["code"], "tool_already_registered",
        "AlreadyRegistered must serialise with #[serde(rename)] code"
    );
}

#[test]
fn registry_error_other_variants_round_trip_with_snake_case_codes() {
    let cases: Vec<(RegistryError, &str)> = vec![
        (
            RegistryError::SessionMismatch {
                token_session: SessionId::new("token").unwrap(),
                reg_session: SessionId::new("reg").unwrap(),
            },
            "session_mismatch",
        ),
        (
            RegistryError::ServerIdCollision {
                server_id: server(),
            },
            "server_id_collision",
        ),
        (
            RegistryError::ServerIdInUse {
                server_id: server(),
            },
            "server_id_in_use",
        ),
        (
            RegistryError::InvalidDescription {
                message: "missing field".to_owned(),
            },
            "invalid_description",
        ),
        (
            RegistryError::StaleGeneration {
                expected: 4,
                actual: 5,
            },
            "stale_generation",
        ),
    ];
    for (err, expected_code) in cases {
        let json = roundtrip(&err);
        assert_eq!(json["code"], json!(expected_code));
    }
}

#[test]
fn method_serialises_with_dot_notation_for_dotted_methods() {
    let cases = [
        (Method::ToolCall, "tool.call"),
        (Method::ToolCancel, "tool.cancel"),
        (Method::ToolNotify, "tool.notify"),
        (Method::ToolNotification, "tool.notification"),
        (Method::ToolsList, "tools.list"),
        (Method::ToolsSearch, "tools.search"),
        (Method::SessionOpen, "session_open"),
        (Method::SessionClose, "session_close"),
        (Method::SessionBindServer, "session_bind_server"),
        (Method::SessionUnbindServer, "session_unbind_server"),
        (Method::SubscribeNotifications, "subscribe_notifications"),
        (
            Method::UnsubscribeNotifications,
            "unsubscribe_notifications",
        ),
        (Method::Hook, "hook"),
        (Method::SubscribeAck, "subscribe_ack"),
        (Method::UnsubscribeAck, "unsubscribe_ack"),
        (Method::ToolsChanged, "tools_changed"),
        (Method::Hello, "hello"),
        (Method::HelloAck, "hello_ack"),
        (Method::ToolCallRequest, "tool_call_request"),
    ];
    for (m, expected) in cases {
        assert_eq!(roundtrip(&m), json!(expected));
        assert_eq!(m.as_wire_str(), expected);
    }
}

#[test]
fn method_round_trips_for_every_variant() {
    for &m in Method::ALL {
        let v = serde_json::to_value(m).expect("serialize");
        let parsed: Method = serde_json::from_value(v.clone()).expect("deserialize");
        assert_eq!(parsed, m);
        // `as_wire_str` and the serde-emitted wire string MUST agree;
        // a future variant added without a matching `as_wire_str` arm
        // would silently drift through the round-trip but be wrong on
        // any code path that relies on the const wire-string lookup.
        let serde_str = v.as_str().expect("variant serialises to a string");
        assert_eq!(
            m.as_wire_str(),
            serde_str,
            "as_wire_str disagrees with serde-emitted wire string for {m:?}"
        );
    }
}

#[test]
fn tool_error_wire_round_trips_every_variant_with_section_15_code() {
    let cases: Vec<(ToolErrorWire, &str)> = vec![
        (
            ToolErrorWire::ToolNotFound { tool_id: tool() },
            "tool_not_found",
        ),
        (ToolErrorWire::SessionMismatch, "session_mismatch"),
        (
            ToolErrorWire::PermissionDenied {
                reason: "missing scope".to_owned(),
            },
            "forbidden",
        ),
        (
            ToolErrorWire::TransportClosed { tool_id: tool() },
            "connection_lost",
        ),
        (
            ToolErrorWire::Timeout {
                tool_id: tool(),
                elapsed_ms: 60_000,
            },
            "timeout",
        ),
        (ToolErrorWire::Cancelled { tool_id: tool() }, "cancelled"),
        (
            ToolErrorWire::InvalidArguments {
                message: "bad arg".to_owned(),
                details: Some(json!({"field": "x"})),
            },
            "invalid_params",
        ),
        (
            ToolErrorWire::Execution {
                tool_id: tool(),
                message: "boom".to_owned(),
            },
            "execution",
        ),
        (
            ToolErrorWire::UnsupportedProtocolVersion {
                supported: vec!["1".to_owned()],
            },
            "unsupported_protocol_version",
        ),
        (
            ToolErrorWire::PayloadTooLarge {
                bytes: 9_000_000,
                limit: 8_388_608,
            },
            "frame_too_large",
        ),
        (
            ToolErrorWire::BehaviorVersionUnsupported {
                tool_id: tool(),
                requested: "v999".to_owned(),
            },
            "behavior_version_unsupported",
        ),
        (
            ToolErrorWire::Internal {
                request_id: Some(RequestId::new("req-1").unwrap()),
                detail: Some("upstream publish failed".to_owned()),
            },
            "internal_error",
        ),
        (
            ToolErrorWire::Custom {
                subcode: "my_err".to_owned(),
                message: "custom oops".to_owned(),
                details: None,
            },
            "custom",
        ),
    ];
    for (err, expected_code) in cases {
        let v = roundtrip(&err);
        assert_eq!(v["code"], json!(expected_code));
    }
}

#[test]
fn tool_error_wire_internal_without_detail_still_parses() {
    // Frames from older peers predate the optional `detail` field and must
    // keep deserializing; `detail: None` must also be omitted on the wire.
    let wire: ToolErrorWire = serde_json::from_value(json!({ "code": "internal_error" }))
        .expect("old internal_error frame parses");
    assert_eq!(
        wire,
        ToolErrorWire::Internal {
            request_id: None,
            detail: None,
        }
    );
    let v = serde_json::to_value(&wire).unwrap();
    assert!(
        !v.as_object().unwrap().contains_key("detail"),
        "detail=None must be omitted: {v}"
    );
}

#[test]
fn tool_error_wire_internal_detail_round_trips() {
    let wire = ToolErrorWire::Internal {
        request_id: None,
        detail: Some("cross-instance tool.call timed out".to_owned()),
    };
    let v = roundtrip(&wire);
    assert_eq!(v["code"], json!("internal_error"));
    assert_eq!(v["detail"], json!("cross-instance tool.call timed out"));
}

#[test]
fn tool_error_wire_invalid_arguments_omits_optional_details() {
    let err = ToolErrorWire::InvalidArguments {
        message: "bad".to_owned(),
        details: None,
    };
    let v = serde_json::to_value(&err).unwrap();
    assert!(
        !v.as_object().unwrap().contains_key("details"),
        "details=None must be omitted: {v}"
    );
}

#[test]
fn tool_output_wire_text_round_trips_with_kind_and_value() {
    let out = ToolOutputWire::Text("hello".to_owned());
    let v = roundtrip(&out);
    assert_eq!(v, json!({"kind": "text", "value": "hello"}));
}

#[test]
fn tool_output_wire_json_round_trips_with_kind_and_value() {
    let out = ToolOutputWire::Json(json!({"x": 1}));
    let v = roundtrip(&out);
    assert_eq!(v, json!({"kind": "json", "value": {"x": 1}}));
}

#[test]
fn tool_output_wire_mcp_round_trips_with_blocks_array() {
    let out = ToolOutputWire::Mcp {
        blocks: vec![
            McpBlock::Text {
                text: "hi".to_owned(),
            },
            McpBlock::Image {
                mime_type: "image/png".to_owned(),
                data: "YWJj".to_owned(),
            },
            McpBlock::Resource {
                uri: "file:///etc/hosts".to_owned(),
                mime_type: Some("text/plain".to_owned()),
                text: None,
            },
        ],
    };
    let v = roundtrip(&out);
    assert_eq!(v["kind"], json!("mcp"));
    let blocks = v["value"]["blocks"].as_array().unwrap();
    assert_eq!(blocks[0], json!({"type": "text", "text": "hi"}));
    assert_eq!(
        blocks[1],
        json!({"type": "image", "mime_type": "image/png", "data": "YWJj"})
    );
    assert_eq!(blocks[2]["type"], json!("resource"));
    assert!(
        !blocks[2].as_object().unwrap().contains_key("text"),
        "text=None must be omitted: {}",
        blocks[2]
    );
}

#[test]
fn mcp_block_resource_omits_optionals_when_none() {
    let r = McpBlock::Resource {
        uri: "u".to_owned(),
        mime_type: None,
        text: None,
    };
    let v = roundtrip(&r);
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("mime_type"));
    assert!(!obj.contains_key("text"));
}

#[test]
fn wire_notification_known_uses_adjacent_tag() {
    let notification_json = json!({
        "type": "BashOutputChunk",
        "tool_call_id": "call_1",
        "command": "ls",
        "output": [97],
        "total_bytes": 1,
        "truncated": false,
        "cwd": "/tmp",
    });
    let wire = WireToolNotification::Known(notification_json);
    let v = roundtrip(&wire);
    assert_eq!(v["shape"], json!("known"));
    assert_eq!(v["value"]["type"], json!("BashOutputChunk"));
    assert_eq!(v["value"]["tool_call_id"], json!("call_1"));
}

#[test]
fn wire_notification_custom_uses_adjacent_tag() {
    let wire = WireToolNotification::Custom(WireCustomNotification {
        kind: "my_tool.progress".to_owned(),
        payload: json!({"pct": 42}),
    });
    let v = roundtrip(&wire);
    assert_eq!(
        v,
        json!({
            "shape": "custom",
            "value": {
                "kind": "my_tool.progress",
                "payload": {"pct": 42},
            },
        })
    );
}

#[test]
fn tool_call_params_round_trips_and_omits_optionals_when_none() {
    let p = ToolCallParams {
        tool_call_id: call_id(),
        tool_id: tool(),
        arguments: json!({"path": "/tmp"}),
        deadline_ms: None,
        behavior_version: None,
        cwd: None,
        trace_context: None,
    };
    let v = roundtrip(&p);
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("deadline_ms"));
    assert!(!obj.contains_key("behavior_version"));
    assert!(!obj.contains_key("cwd"));
    assert!(!obj.contains_key("trace_context"));

    let full = ToolCallParams {
        tool_call_id: call_id(),
        tool_id: tool(),
        arguments: json!({}),
        deadline_ms: Some(60_000),
        behavior_version: Some("v1".to_owned()),
        cwd: Some("/work".to_owned()),
        trace_context: Some("00-trace-span-01".to_owned()),
    };
    let v = roundtrip(&full);
    assert_eq!(v["deadline_ms"], json!(60_000));
    assert_eq!(v["behavior_version"], json!("v1"));
    assert_eq!(v["cwd"], json!("/work"));
    assert_eq!(v["trace_context"], json!("00-trace-span-01"));
}

#[test]
fn tool_call_result_omits_empty_follow_up_and_reminder_arrays() {
    let r = ToolCallResult {
        tool_call_id: call_id(),
        output: ToolOutputWire::Text("done".to_owned()),
        follow_ups: vec![],
        reminders: vec![],
        chat_completion_output: None,
    };
    let v = roundtrip(&r);
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("follow_ups"));
    assert!(!obj.contains_key("reminders"));
    assert!(!obj.contains_key("chat_completion_output"));
    assert_eq!(v["output"]["kind"], json!("text"));
}

#[test]
fn tool_call_result_carries_chat_completion_output_when_present() {
    let cco = json!({
        "result": {
            "sender": "assistant",
            "message": "",
            "code_execution_result": {
                "stdout": "hello\n",
                "stderr": "",
                "exit_code": 0,
                "command_timed_out": false
            }
        }
    });
    let r = ToolCallResult {
        tool_call_id: call_id(),
        output: ToolOutputWire::Json(json!({"stdout": "hello\n"})),
        follow_ups: vec![],
        reminders: vec![],
        chat_completion_output: Some(cco.clone()),
    };
    let v = roundtrip(&r);
    assert_eq!(v["chat_completion_output"], cco);
}

#[test]
fn tool_call_result_without_chat_completion_output_key_defaults_to_none() {
    let legacy = json!({
        "tool_call_id": "call_alpha",
        "output": {"kind": "text", "value": "done"}
    });
    let parsed: ToolCallResult =
        serde_json::from_value(legacy).expect("legacy payload without field deserializes");
    assert!(parsed.chat_completion_output.is_none());
    assert!(parsed.follow_ups.is_empty());
    assert!(parsed.reminders.is_empty());
}

#[test]
fn tool_call_progress_frame_round_trips() {
    let p = ToolCallProgressFrame {
        tool_call_id: call_id(),
        kind: "chunk".to_owned(),
        body: json!({"offset": 0, "total": 3, "data_b64": "YWJj"}),
        dropped_count: Some(2),
    };
    let v = roundtrip(&p);
    assert_eq!(v["dropped_count"], json!(2));
}

#[test]
fn tool_notification_frame_round_trips_with_optional_call_id() {
    let f = ToolNotificationFrame {
        tool_call_id: Some(call_id()),
        tool_id: Some(tool()),
        notification: WireToolNotification::Custom(WireCustomNotification {
            kind: "x".to_owned(),
            payload: json!({}),
        }),
    };
    roundtrip(&f);
}

#[test]
fn tool_registration_round_trips() {
    let reg = ToolRegistration {
        tool_id: tool(),
        sessions: Some(vec![session()]),
        user_id: user(),
        server_id: None,
        description: ToolDescription::new("read_file", "d").with_namespace("EchoAgentBuild"),
        input_schema: None,
        capabilities: None,
        notification_schemas: None,
        transport_kind: TransportKind::Remote,
        if_match_generation: None,
        metadata: None,
    };
    roundtrip(&reg);
}

#[test]
fn tools_list_and_search_payloads_round_trip() {
    roundtrip(&ToolsListParams {
        session_id: session(),
        mode: ToolDefinitionMode::Concise {
            meta_search: ToolId::new("search_tool").unwrap(),
            meta_call: ToolId::new("use_tool").unwrap(),
        },
    });
    roundtrip(&ToolsListResult {
        tools: vec![ToolDescription::new("a", "b")],
    });
    roundtrip(&ToolsSearchParams {
        session_id: session(),
        query: "q".to_owned(),
        limit: 10,
    });
    roundtrip(&ToolsSearchResultBody {
        results: vec![ToolSearchResult {
            tool_name: "t".to_owned(),
            server_name: "s".to_owned(),
            description: "d".to_owned(),
            score: 0.5,
            parameters: vec!["p".to_owned()],
            input_schema: json!({}),
        }],
        total_hidden_tools: 7,
        is_ready: true,
    });
}

#[test]
fn session_lifecycle_payloads_round_trip() {
    let open_no_resume = SessionOpenParams {
        resume: false,
        last_seq: None,
    };
    let v = roundtrip(&open_no_resume);
    assert_eq!(v["resume"], json!(false));
    assert!(!v.as_object().unwrap().contains_key("last_seq"));
    assert!(
        !v.as_object().unwrap().contains_key("session_id"),
        "session_id must not appear in SessionOpenParams (it belongs on the envelope)"
    );
    assert!(
        !v.as_object().unwrap().contains_key("server_id"),
        "server_id must not appear in SessionOpenParams (moved to session_bind_server)"
    );

    let open_resume = SessionOpenParams {
        resume: true,
        last_seq: Some(LastSeq {
            connection_id: ConnectionId::new("c1").unwrap(),
            seq: FrameSeq::new(99),
        }),
    };
    let v = roundtrip(&open_resume);
    assert_eq!(v["last_seq"]["seq"], json!(99));

    let close = SessionCloseParams {
        reason: Some("user closed".to_owned()),
    };
    roundtrip(&close);

    // session_bind_server / session_unbind_server
    let bind = SessionBindServerParams {
        server_id: server(),
        cwd: Some("/tmp/test".to_owned()),
        metadata: Some(json!({"key": "value"})),
    };
    let v = roundtrip(&bind);
    assert_eq!(v["server_id"], json!("srv-uuidv7"));
    assert_eq!(v["cwd"], json!("/tmp/test"));
    assert_eq!(v["metadata"]["key"], json!("value"));

    let bind_result = SessionBindServerResult {
        tools: vec![ToolDescription::new("my_tool", "desc")],
        binary_version: Some("1.0.15".to_owned()),
        unserved_tool_ids: vec!["EchoAgentBuild:monitor".to_owned()],
        resolve_error: Some("missing_tool_config: no explicit tool configuration".to_owned()),
        image_capabilities: vec![IMAGE_CAPABILITIES_V1.to_owned(), "node.22".to_owned()],
    };
    let v = roundtrip(&bind_result);
    assert_eq!(v["tools"].as_array().unwrap().len(), 1);
    assert_eq!(v["binary_version"], json!("1.0.15"));
    assert_eq!(v["unserved_tool_ids"], json!(["EchoAgentBuild:monitor"]));
    assert_eq!(
        v["resolve_error"],
        json!("missing_tool_config: no explicit tool configuration")
    );
    assert_eq!(
        v["image_capabilities"],
        json!(["capabilities.v1", "node.22"])
    );

    let bind_result_empty = SessionBindServerResult::default();
    let v = roundtrip(&bind_result_empty);
    assert!(
        !v.as_object().unwrap().contains_key("tools"),
        "empty tools must be omitted from wire"
    );
    assert!(
        !v.as_object().unwrap().contains_key("binary_version")
            && !v.as_object().unwrap().contains_key("unserved_tool_ids")
            && !v.as_object().unwrap().contains_key("resolve_error")
            && !v.as_object().unwrap().contains_key("image_capabilities"),
        "absent bind-report fields must be omitted from wire (old-server parity)"
    );

    let legacy: SessionBindServerResult =
        serde_json::from_value(json!({"tools": []})).expect("legacy payload parses");
    assert_eq!(legacy.binary_version, None);
    assert!(legacy.unserved_tool_ids.is_empty());
    assert_eq!(legacy.resolve_error, None);
    assert!(legacy.image_capabilities.is_empty());

    let unbind = SessionUnbindServerParams {
        server_id: server(),
    };
    let v = roundtrip(&unbind);
    assert_eq!(v["server_id"], json!("srv-uuidv7"));

    let attach = SessionAttachServerParams {
        server_id: Some(server()),
        caller: Some("fs_read".to_owned()),
    };
    let v = roundtrip(&attach);
    assert_eq!(v["server_id"], json!("srv-uuidv7"));
    assert_eq!(v["caller"], json!("fs_read"));
    let v = roundtrip(&SessionAttachServerParams::default());
    assert_eq!(v, json!({}), "absent attach params must be omitted");

    let attach_result = SessionAttachServerResult {
        tools: vec![ToolDescription::new("my_tool", "desc")],
        route: Some(AttachRoute::Local),
    };
    let v = roundtrip(&attach_result);
    assert_eq!(v["tools"].as_array().unwrap().len(), 1);
    assert_eq!(v["route"], json!("local"));
    let v = roundtrip(&SessionAttachServerResult::default());
    assert!(
        !v.as_object().unwrap().contains_key("tools")
            && !v.as_object().unwrap().contains_key("route"),
        "empty attach result fields must be omitted from wire"
    );
}

#[test]
fn session_bind_result_image_capabilities_round_trip() {
    let populated = SessionBindResult {
        tools: vec![ToolDescription::new("my_tool", "desc")],
        binary_version: Some("1.0.15".to_owned()),
        unserved_tool_ids: Vec::new(),
        resolve_error: None,
        image_capabilities: vec![
            IMAGE_CAPABILITIES_V1.to_owned(),
            "echo-agent-files.occ".to_owned(),
        ],
    };
    let v = roundtrip(&populated);
    assert_eq!(
        v["image_capabilities"],
        json!(["capabilities.v1", "echo-agent-files.occ"])
    );

    let v = roundtrip(&SessionBindResult::default());
    assert!(
        !v.as_object().unwrap().contains_key("image_capabilities"),
        "an empty token set must be omitted (old-server parity)"
    );

    // A server predating the field.
    let legacy: SessionBindResult =
        serde_json::from_value(json!({"tools": []})).expect("legacy payload parses");
    assert!(legacy.image_capabilities.is_empty());
}

#[test]
fn attach_route_round_trips_snake_case_and_tolerates_unknown() {
    assert_eq!(roundtrip(&AttachRoute::Local), json!("local"));
    assert_eq!(roundtrip(&AttachRoute::Remote), json!("remote"));
    // "restored" was removed with restore-on-activity; old hubs may still send
    // it, and it must fall into the tolerant `Unknown` bucket like any other
    // retired/newer value.
    let parsed: AttachRoute =
        serde_json::from_value(json!("restored")).expect("tolerant parse of retired value");
    assert_eq!(parsed, AttachRoute::Unknown);
    let parsed: AttachRoute =
        serde_json::from_value(json!("route_from_a_newer_hub")).expect("tolerant parse");
    assert_eq!(parsed, AttachRoute::Unknown);
}

#[test]
fn subscriptions_round_trip() {
    let sub_no_filter = SubscribeNotificationsParams {
        session_id: session(),
        filter: None,
    };
    roundtrip(&sub_no_filter);

    let sub = SubscribeNotificationsParams {
        session_id: session(),
        filter: Some(NotificationFilter {
            tool_id: Some(tool()),
            kinds: Some(vec!["BashOutputChunk".to_owned()]),
        }),
    };
    roundtrip(&sub);

    // Exhaustive coverage of every `SubscribeOutcome` and
    // `UnsubscribeOutcome` variant. The `match` discriminant ensures
    // the compiler flags any future variant addition that this loop
    // forgets to round-trip.
    fn subscribe_wire_str(o: SubscribeOutcome) -> &'static str {
        match o {
            SubscribeOutcome::Subscribed => "subscribed",
            SubscribeOutcome::AlreadySubscribed => "already_subscribed",
            SubscribeOutcome::NotAuthorized => "not_authorized",
        }
    }
    for outcome in [
        SubscribeOutcome::Subscribed,
        SubscribeOutcome::AlreadySubscribed,
        SubscribeOutcome::NotAuthorized,
    ] {
        let v = roundtrip(&SubscribeAck {
            outcome,
            subscription_id: "sub_1".to_owned(),
        });
        assert_eq!(v["outcome"], json!(subscribe_wire_str(outcome)));
    }

    fn unsubscribe_wire_str(o: UnsubscribeOutcome) -> &'static str {
        match o {
            UnsubscribeOutcome::Unsubscribed => "unsubscribed",
            UnsubscribeOutcome::NotSubscribed => "not_subscribed",
            UnsubscribeOutcome::Evicted => "evicted",
        }
    }
    for outcome in [
        UnsubscribeOutcome::Unsubscribed,
        UnsubscribeOutcome::NotSubscribed,
        UnsubscribeOutcome::Evicted,
    ] {
        let v = roundtrip(&UnsubscribeAck {
            outcome,
            subscription_id: "sub_1".to_owned(),
        });
        assert_eq!(v["outcome"], json!(unsubscribe_wire_str(outcome)));
    }

    roundtrip(&UnsubscribeNotificationsParams {
        session_id: session(),
        subscription_id: "sub_1".to_owned(),
    });
}

#[test]
fn server_bind_outcome_variants_round_trip_with_wire_strings() {
    fn wire_str(o: ServerBindOutcome) -> &'static str {
        match o {
            ServerBindOutcome::Bound => "bound",
            ServerBindOutcome::AlreadyBound => "already_bound",
            ServerBindOutcome::ServerNotFound => "server_not_found",
            ServerBindOutcome::Unavailable => "unavailable",
        }
    }
    for outcome in [
        ServerBindOutcome::Bound,
        ServerBindOutcome::AlreadyBound,
        ServerBindOutcome::ServerNotFound,
        ServerBindOutcome::Unavailable,
    ] {
        let v = roundtrip(&ServerBindAck { outcome });
        assert_eq!(v["outcome"], json!(wire_str(outcome)));
    }
    assert_ne!(
        wire_str(ServerBindOutcome::Unavailable),
        wire_str(ServerBindOutcome::ServerNotFound),
        "unavailable must be wire-distinct from server_not_found",
    );
}

#[test]
fn hook_frame_round_trips_with_optional_fields() {
    let f = HookFrame {
        session_id: session(),
        tool_id: Some(tool()),
        call_id: Some(call_id()),
        hook_id: None,
        event: HookEvent::Cancel,
        trace_context: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_owned()),
    };
    let v = roundtrip(&f);
    assert_eq!(v["event"]["type"], json!("Cancel"));
    assert_eq!(
        v["trace_context"],
        json!("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
    );

    let session_wide = HookFrame {
        session_id: session(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::SessionEnded,
        trace_context: None,
    };
    let v = roundtrip(&session_wide);
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("tool_id"));
    assert!(!obj.contains_key("call_id"));
    assert!(!obj.contains_key("trace_context"));
}

#[test]
fn session_event_turn_started_round_trips_via_re_export() {
    let event = SessionEvent::TurnStarted {
        turn_number: 10,
        model_id: "echo-agent-3".into(),
        yolo_mode: true,
    };
    let v = roundtrip(&event);
    assert_eq!(v["event_type"], json!("turn_started"));
}

#[test]
fn session_event_unknown_round_trips_via_re_export() {
    let v = json!({"event_type": "future_thing"});
    let event: SessionEvent = serde_json::from_value(v).unwrap();
    assert_eq!(event, SessionEvent::Unknown);
}

#[test]
fn tool_call_outcome_round_trips_via_re_export() {
    assert_eq!(roundtrip(&ToolCallOutcome::Success), json!("success"));
    assert_eq!(roundtrip(&ToolCallOutcome::Error), json!("error"));
    assert_eq!(roundtrip(&ToolCallOutcome::Cancelled), json!("cancelled"));
}

#[test]
fn session_phase_round_trips_via_re_export() {
    assert_eq!(roundtrip(&SessionPhase::Idle), json!("idle"));
    assert_eq!(roundtrip(&SessionPhase::Sampling), json!("sampling"));
    assert_eq!(
        roundtrip(&SessionPhase::ToolExecution),
        json!("tool_execution")
    );
    assert_eq!(
        roundtrip(&SessionPhase::PermissionPrompt),
        json!("permission_prompt")
    );
}

#[test]
fn tools_changed_round_trips_with_per_array_skip_when_empty() {
    let empty = ToolsChanged {
        session_id: session(),
        added: vec![],
        removed: vec![],
        updated: vec![],
    };
    let v = roundtrip(&empty);
    let obj = v.as_object().unwrap();
    assert!(!obj.contains_key("added"));
    assert!(!obj.contains_key("removed"));
    assert!(!obj.contains_key("updated"));

    let populated = ToolsChanged {
        session_id: session(),
        added: vec![tool()],
        removed: vec![],
        updated: vec![tool()],
    };
    let v = roundtrip(&populated);
    assert_eq!(v["added"][0], json!("EchoAgentBuild:read_file"));
    assert_eq!(v["updated"][0], json!("EchoAgentBuild:read_file"));
    assert!(!v.as_object().unwrap().contains_key("removed"));
}

#[test]
fn ping_pong_frames_round_trip() {
    roundtrip(&PingFrame::new(1_700_000_000_000));
    roundtrip(&PongFrame::new(1_700_000_000_500));
}

#[test]
fn ping_frame_serializes_with_method() {
    let frame = PingFrame::new(1_700_000_000_000);
    let v = serde_json::to_value(frame).expect("serialize");
    assert_eq!(
        v["method"], "ping",
        "PingFrame must include method on the wire"
    );
    assert_eq!(v["ts_ms"], 1_700_000_000_000u64);
}

#[test]
fn pong_frame_serializes_with_method() {
    let frame = PongFrame::new(1_700_000_000_500);
    let v = serde_json::to_value(frame).expect("serialize");
    assert_eq!(
        v["method"], "pong",
        "PongFrame must include method on the wire"
    );
    assert_eq!(v["ts_ms"], 1_700_000_000_500u64);
}

#[test]
fn ping_frame_deserializes_without_method() {
    // Backwards compat: old builds sent {"ts_ms": N} without method.
    let old_format = json!({"ts_ms": 42});
    let frame: PingFrame = serde_json::from_value(old_format).expect("deserialize");
    assert_eq!(frame.ts_ms, 42);
}

#[test]
fn pong_frame_deserializes_without_method() {
    let old_format = json!({"ts_ms": 99});
    let frame: PongFrame = serde_json::from_value(old_format).expect("deserialize");
    assert_eq!(frame.ts_ms, 99);
}

#[test]
fn ping_frame_deserializes_with_method() {
    let new_format = json!({"method": "ping", "ts_ms": 42});
    let frame: PingFrame = serde_json::from_value(new_format).expect("deserialize");
    assert_eq!(frame.ts_ms, 42);
}

#[test]
fn ping_frame_rejects_wrong_method() {
    let wrong = json!({"method": "pong", "ts_ms": 1});
    let err = serde_json::from_value::<PingFrame>(wrong);
    assert!(err.is_err(), "PingFrame must reject method:\"pong\"");
}

#[test]
fn pong_frame_rejects_wrong_method() {
    let wrong = json!({"method": "ping", "ts_ms": 1});
    let err = serde_json::from_value::<PongFrame>(wrong);
    assert!(err.is_err(), "PongFrame must reject method:\"ping\"");
}

#[test]
fn error_codes_are_bijective() {
    let mut numerics: Vec<i32> = ERROR_CODES.iter().map(|(n, _)| *n).collect();
    numerics.sort_unstable();
    let unique: Vec<i32> = {
        let mut v = numerics.clone();
        v.dedup();
        v
    };
    assert_eq!(unique.len(), numerics.len(), "numeric codes must be unique");

    let mut strings: Vec<&str> = ERROR_CODES.iter().map(|(_, s)| *s).collect();
    strings.sort_unstable();
    let unique: Vec<&str> = {
        let mut v = strings.clone();
        v.dedup();
        v
    };
    assert_eq!(unique.len(), strings.len(), "string codes must be unique");
}

#[test]
fn error_codes_numeric_and_string_are_inverses() {
    for (n, s) in ERROR_CODES {
        assert_eq!(
            error_codes::numeric_for(s),
            Some(*n),
            "string {s:?} → numeric"
        );
        assert_eq!(
            error_codes::string_for(*n),
            Some(*s),
            "numeric {n} → string"
        );
    }
    assert_eq!(error_codes::numeric_for("not_a_real_code"), None);
    assert_eq!(error_codes::string_for(0), None);
}

#[test]
fn from_tool_error_wire_maps_each_variant_to_its_numeric() {
    use error_codes::from_tool_error_wire as fe;
    assert_eq!(fe(&ToolErrorWire::ToolNotFound { tool_id: tool() }), -32011);
    assert_eq!(fe(&ToolErrorWire::SessionMismatch), -32600);
    assert_eq!(
        fe(&ToolErrorWire::PermissionDenied {
            reason: String::new()
        }),
        -32003
    );
    assert_eq!(
        fe(&ToolErrorWire::TransportClosed { tool_id: tool() }),
        -32004
    );
    assert_eq!(
        fe(&ToolErrorWire::Timeout {
            tool_id: tool(),
            elapsed_ms: 0
        }),
        -32001
    );
    assert_eq!(
        fe(&ToolErrorWire::InvalidArguments {
            message: String::new(),
            details: None
        }),
        -32602
    );
    assert_eq!(
        fe(&ToolErrorWire::PayloadTooLarge { bytes: 0, limit: 0 }),
        -32018
    );
    assert_eq!(
        fe(&ToolErrorWire::BehaviorVersionUnsupported {
            tool_id: tool(),
            requested: String::new()
        }),
        -32020
    );
    assert_eq!(
        fe(&ToolErrorWire::UnsupportedProtocolVersion { supported: vec![] }),
        -32605
    );
}

#[test]
fn known_notification_kinds_include_signature_variants() {
    for required in [
        "BashOutputChunk",
        "BashExecutionComplete",
        "FileWritten",
        "MonitorEvent",
        "TaskCompleted",
        "LspServerReady",
        "ScheduledTaskFired",
        "PlanModeEntered",
        "UserQuestionAsked",
    ] {
        assert!(
            KNOWN_NOTIFICATION_KINDS.contains(&required),
            "missing required notification kind: {required}"
        );
    }
}

#[test]
fn jsonrpc_version_round_trips_only_for_2_0() {
    let v = serde_json::to_value(JsonRpcVersion).unwrap();
    assert_eq!(v, json!("2.0"));
    let _: JsonRpcVersion = serde_json::from_value(json!("2.0")).unwrap();
    for bad in [json!("1.0"), json!("3.0"), json!(2.0), json!(null)] {
        assert!(
            serde_json::from_value::<JsonRpcVersion>(bad.clone()).is_err(),
            "must reject {bad}"
        );
    }
}

#[test]
fn jsonrpc_id_string_and_number_both_round_trip() {
    let sid = JsonRpcId::new_string("req-1");
    assert_eq!(roundtrip(&sid), json!("req-1"));

    let nid = JsonRpcId::Number(42);
    assert_eq!(roundtrip(&nid), json!(42));

    let parsed: JsonRpcId = serde_json::from_value(json!(7)).unwrap();
    assert_eq!(parsed, JsonRpcId::Number(7));
    let parsed: JsonRpcId = serde_json::from_value(json!("x")).unwrap();
    assert_eq!(parsed, JsonRpcId::String("x".to_owned()));
}

#[test]
fn tool_scope_serialises_snake_case() {
    assert_eq!(roundtrip(&ToolScope::Read), json!("read"));
    assert_eq!(roundtrip(&ToolScope::Write), json!("write"));
}

#[test]
fn tool_capabilities_with_tool_scope_round_trips_with_literal_value() {
    let caps = ToolCapabilities {
        tool_scope: Some(ToolScope::Write),
        ..Default::default()
    };
    let v = roundtrip(&caps);
    assert_eq!(v["tool_scope"], json!("write"));
}

#[test]
fn tool_definition_mode_full_serialises_as_object_with_mode_key() {
    let v = roundtrip(&ToolDefinitionMode::Full);
    assert_eq!(v, json!({"mode": "full"}));
}

#[test]
fn tool_definition_mode_concise_carries_meta_tool_pair() {
    let mode = ToolDefinitionMode::Concise {
        meta_search: ToolId::new("search_tool").unwrap(),
        meta_call: ToolId::new("use_tool").unwrap(),
    };
    let v = roundtrip(&mode);
    assert_eq!(
        v,
        json!({
            "mode": "concise",
            "meta_search": "search_tool",
            "meta_call": "use_tool",
        })
    );
}

#[test]
fn tool_definition_mode_concise_supports_alternate_meta_tool_pair() {
    let mode = ToolDefinitionMode::Concise {
        meta_search: ToolId::new("search_connected_tools").unwrap(),
        meta_call: ToolId::new("call_connected_tool").unwrap(),
    };
    let v = roundtrip(&mode);
    assert_eq!(v["mode"], json!("concise"));
    assert_eq!(v["meta_search"], json!("search_connected_tools"));
    assert_eq!(v["meta_call"], json!("call_connected_tool"));
}

#[test]
fn tool_error_wire_render_limited_round_trips_with_card_id() {
    let err = ToolErrorWire::RenderLimited {
        tool_id: ToolId::new("render:imagine_video").unwrap(),
        card_id: Some("card-uuid-123".to_owned()),
        reason: "budget exceeded".to_owned(),
    };
    let v = roundtrip(&err);
    assert_eq!(v["code"], json!("render_limited"));
    assert_eq!(v["tool_id"], json!("render:imagine_video"));
    assert_eq!(v["card_id"], json!("card-uuid-123"));
    assert_eq!(v["reason"], json!("budget exceeded"));
}

#[test]
fn tool_error_wire_render_limited_omits_card_id_when_none() {
    let err = ToolErrorWire::RenderLimited {
        tool_id: ToolId::new("render:imagine_video").unwrap(),
        card_id: None,
        reason: "budget exceeded".to_owned(),
    };
    let v = serde_json::to_value(&err).unwrap();
    assert!(
        !v.as_object().unwrap().contains_key("card_id"),
        "card_id=None must be omitted: {v}",
    );
}

#[test]
fn tool_error_wire_terminal_error_round_trips_with_string_code() {
    let err = ToolErrorWire::TerminalError {
        tool_id: ToolId::new("EchoAgentBuild:bash").unwrap(),
        message: "exit 137".to_owned(),
    };
    let v = roundtrip(&err);
    assert_eq!(v["code"], json!("terminal_error"));
    assert_eq!(v["tool_id"], json!("EchoAgentBuild:bash"));
    assert_eq!(v["message"], json!("exit 137"));
}

#[test]
fn error_codes_table_includes_render_limited_and_terminal_error() {
    assert_eq!(error_codes::numeric_for("render_limited"), Some(-32023));
    assert_eq!(error_codes::numeric_for("terminal_error"), Some(-32024));
    assert_eq!(error_codes::string_for(-32023), Some("render_limited"));
    assert_eq!(error_codes::string_for(-32024), Some("terminal_error"));
}

#[test]
fn from_tool_error_wire_maps_render_limited_and_terminal_error() {
    use error_codes::from_tool_error_wire as fe;
    assert_eq!(
        fe(&ToolErrorWire::RenderLimited {
            tool_id: tool(),
            card_id: None,
            reason: String::new(),
        }),
        -32023,
    );
    assert_eq!(
        fe(&ToolErrorWire::TerminalError {
            tool_id: tool(),
            message: String::new(),
        }),
        -32024,
    );
}

/// Every variant of [`ToolErrorWire`], constructed once with placeholder
/// data. Adding a new variant upstream requires extending this helper
/// too: [`one_of_each_tool_error_wire_variant_is_exhaustive`] asserts
/// `len() == EXPECTED_VARIANT_COUNT`. The compiler does NOT enforce
/// exhaustiveness for `Vec<...>` constructors, so the count check is the
/// explicit guard.
fn one_of_each_tool_error_wire_variant() -> Vec<ToolErrorWire> {
    vec![
        ToolErrorWire::ToolNotFound { tool_id: tool() },
        ToolErrorWire::SessionMismatch,
        ToolErrorWire::PermissionDenied {
            reason: "missing scope".to_owned(),
        },
        ToolErrorWire::TransportClosed { tool_id: tool() },
        ToolErrorWire::Timeout {
            tool_id: tool(),
            elapsed_ms: 0,
        },
        ToolErrorWire::Cancelled { tool_id: tool() },
        ToolErrorWire::InvalidArguments {
            message: "bad arg".to_owned(),
            details: None,
        },
        ToolErrorWire::Execution {
            tool_id: tool(),
            message: "boom".to_owned(),
        },
        ToolErrorWire::UnsupportedProtocolVersion {
            supported: vec!["1".to_owned()],
        },
        ToolErrorWire::PayloadTooLarge {
            bytes: 9_000_000,
            limit: 8_388_608,
        },
        ToolErrorWire::BehaviorVersionUnsupported {
            tool_id: tool(),
            requested: "v999".to_owned(),
        },
        ToolErrorWire::RenderLimited {
            tool_id: tool(),
            card_id: None,
            reason: "budget exceeded".to_owned(),
        },
        ToolErrorWire::TerminalError {
            tool_id: tool(),
            message: "exit 137".to_owned(),
        },
        ToolErrorWire::Internal {
            request_id: Some(RequestId::new("req-1").unwrap()),
            detail: Some("upstream publish failed".to_owned()),
        },
        ToolErrorWire::Custom {
            subcode: "my_err".to_owned(),
            message: "custom oops".to_owned(),
            details: None,
        },
    ]
}

const EXPECTED_VARIANT_COUNT: usize = 15;

#[test]
fn one_of_each_tool_error_wire_variant_is_exhaustive() {
    assert_eq!(
        one_of_each_tool_error_wire_variant().len(),
        EXPECTED_VARIANT_COUNT,
        "variant fixture out of sync with EXPECTED_VARIANT_COUNT — also \
         update one_of_each_tool_error_wire_variant() and the audit allow-list",
    );
}

/// Wire-string discriminators that deliberately do NOT have a row in the
/// numeric ↔ string table. Each rides on a generic JSON-RPC reserved
/// numeric (`invalid_request` or `internal_error`) while emitting a
/// more-specific subcode for receivers that want richer dispatch.
const NON_TABLE_WIRE_STRINGS: &[&str] = &["session_mismatch", "cancelled", "execution", "custom"];

/// For every variant whose wire `data.code` is a table row: assert the
/// strict round-trip (`numeric_for(code) == Some(from_tool_error_wire(&v))`
/// AND `string_for(numeric) == Some(code)`). For variants whose wire
/// `data.code` is in [`NON_TABLE_WIRE_STRINGS`]: `numeric_for` must
/// return `None`, and `from_tool_error_wire` must still return some valid
/// table numeric. Adding a new variant without the matching
/// `#[serde(rename = "...")]` (or without listing the wire string in
/// [`NON_TABLE_WIRE_STRINGS`]) fails this test.
#[test]
fn every_tool_error_wire_variant_aligns_with_codes_table() {
    for err in one_of_each_tool_error_wire_variant() {
        let wire = serde_json::to_value(&err).expect("serialize");
        let code_str = wire["code"]
            .as_str()
            .unwrap_or_else(|| panic!("variant has no `code` discriminator: {wire}"))
            .to_owned();
        let numeric = error_codes::from_tool_error_wire(&err);
        assert!(
            error_codes::string_for(numeric).is_some(),
            "variant {err:?} maps to {numeric} which is not in the codes table",
        );
        if NON_TABLE_WIRE_STRINGS.contains(&code_str.as_str()) {
            assert_eq!(
                error_codes::numeric_for(&code_str),
                None,
                "wire string {code_str:?} is in NON_TABLE_WIRE_STRINGS \
                 but also appears in the codes table — remove from the \
                 allow-list or rename the variant",
            );
        } else {
            assert_eq!(
                error_codes::numeric_for(&code_str),
                Some(numeric),
                "variant {err:?} emits {code_str:?} which is not paired \
                 with {numeric} — add `#[serde(rename = \"<table-string>\")]` \
                 or list {code_str:?} in NON_TABLE_WIRE_STRINGS with a comment",
            );
            assert_eq!(
                error_codes::string_for(numeric),
                Some(code_str.as_str()),
                "inverse failed for {code_str:?} ↔ {numeric}",
            );
        }
    }
}

#[test]
fn tool_registration_with_empty_sessions_omits_field_on_wire() {
    // Sessions field has three-state semantics. None means "no change";
    // Some(vec![]) means "explicit unbind all". Both round-trip
    // correctly, but only None is field-omitted on the wire.
    let omitted = ToolRegistration {
        tool_id: tool(),
        sessions: None,
        user_id: user(),
        server_id: None,
        description: ToolDescription::new("echo", "d"),
        input_schema: None,
        capabilities: None,
        notification_schemas: None,
        transport_kind: TransportKind::Remote,
        if_match_generation: None,
        metadata: None,
    };
    let json = roundtrip(&omitted);
    assert!(
        !json.as_object().unwrap().contains_key("sessions"),
        "sessions=None must be omitted from JSON: {json}"
    );

    let explicit_empty = ToolRegistration {
        sessions: Some(Vec::new()),
        ..omitted
    };
    let json = roundtrip(&explicit_empty);
    let sessions = json
        .as_object()
        .unwrap()
        .get("sessions")
        .expect("sessions=Some(vec![]) must serialise as an explicit array");
    assert!(
        sessions.is_array() && sessions.as_array().unwrap().is_empty(),
        "sessions=Some(vec![]) serialises to an empty JSON array: {json}"
    );
}
