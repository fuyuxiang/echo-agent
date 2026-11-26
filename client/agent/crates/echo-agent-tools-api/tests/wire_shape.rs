//! Serde wire-shape pin tests.
//!
//! `ToolConfigEntry` is serialized into session-bind metadata and backend
//! JSONB config storage. These tests pin the exact JSON shape so a
//! field rename/retype in `echo-agent-tools.proto` cannot silently break those
//! wire contracts (the producer and consumer live in separate services).

use echo_agent_tools_api::ToolConfigEntry;

fn full_entry() -> ToolConfigEntry {
    ToolConfigEntry {
        id: "EchoAgentBuild:grep".to_owned(),
        params_json: Some(r#"{"max_results":50}"#.to_owned()),
        name_override: Some("search".to_owned()),
        params_name_overrides: std::collections::HashMap::from([(
            "pattern".to_owned(),
            "query".to_owned(),
        )]),
        behavior_version: Some("legacy-0.4.10".to_owned()),
        description_override: Some("Search the codebase".to_owned()),
    }
}

fn finalize_request_minimal() -> echo_agent_tools_api::FinalizeToolServerConfigRequest {
    echo_agent_tools_api::FinalizeToolServerConfigRequest {
        tools: vec![],
        truncation: None,
        system_reminders_enabled: false,
        initial_tool_state_json: None,
        behavior_preset: None,
        client_callback_addr: None,
        session_id: None,
        client_callback_secret: None,
    }
}

#[test]
fn tool_config_entry_serializes_to_pinned_json_shape() {
    let value = serde_json::to_value(full_entry()).expect("serialize");
    assert_eq!(
        value,
        serde_json::json!({
            "id": "EchoAgentBuild:grep",
            "params_json": "{\"max_results\":50}",
            "name_override": "search",
            "params_name_overrides": {"pattern": "query"},
            "behavior_version": "legacy-0.4.10",
            "description_override": "Search the codebase",
        })
    );
}

#[test]
fn tool_config_entry_round_trips() {
    let entry = full_entry();
    let json = serde_json::to_string(&entry).expect("serialize");
    let back: ToolConfigEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, entry);
}

#[test]
fn minimal_entry_deserializes_from_id_only() {
    // Consumers must accept sparse payloads: optional fields absent, map empty.
    let back: ToolConfigEntry =
        serde_json::from_value(serde_json::json!({"id": "EchoAgentBuild:read_file"}))
            .expect("deserialize minimal");
    assert_eq!(back.id, "EchoAgentBuild:read_file");
    assert_eq!(back.params_json, None);
    assert_eq!(back.name_override, None);
    assert!(back.params_name_overrides.is_empty());
    assert_eq!(back.behavior_version, None);
    assert_eq!(back.description_override, None);
}

#[test]
fn missing_id_fails_to_deserialize() {
    // `id` is the only required field: a payload without it must be rejected
    // instead of silently deserializing with an empty id.
    let result: Result<ToolConfigEntry, _> =
        serde_json::from_value(serde_json::json!({"name_override": "search"}));
    assert!(result.is_err(), "payload without `id` must be rejected");
}

#[test]
fn explicit_null_optional_fields_deserialize_as_none() {
    // `#[serde(default)]` covers *absent* keys; explicit `null` is handled by
    // the `Option` fields themselves. Pin that both shapes are accepted.
    let back: ToolConfigEntry = serde_json::from_value(serde_json::json!({
        "id": "EchoAgentBuild:read_file",
        "params_json": null,
        "name_override": null,
        "behavior_version": null,
        "description_override": null,
    }))
    .expect("deserialize explicit nulls");
    assert_eq!(back.params_json, None);
    assert_eq!(back.name_override, None);
    assert_eq!(back.behavior_version, None);
    assert_eq!(back.description_override, None);
}

#[test]
fn explicit_null_map_is_rejected() {
    // The map field is not `Option`-typed: `null` is not coerced to an empty
    // map. Producers must omit the key or emit `{}`. Pin the rejection so a
    // codegen change that silently starts accepting `null` is caught.
    let result: Result<ToolConfigEntry, _> = serde_json::from_value(serde_json::json!({
        "id": "EchoAgentBuild:read_file",
        "params_name_overrides": null,
    }));
    assert!(
        result.is_err(),
        "null params_name_overrides must be rejected (omit the key or send {{}})"
    );
}

#[test]
fn finalize_request_callback_fields_are_optional_and_snake_case() {
    let mut req = finalize_request_minimal();
    req.client_callback_addr = Some("http://127.0.0.1:50051".to_owned());
    req.session_id = Some("session-123".to_owned());
    req.client_callback_secret = Some("secret-123".to_owned());

    let value = serde_json::to_value(&req).expect("serialize");
    assert_eq!(
        value.get("client_callback_addr"),
        Some(&serde_json::json!("http://127.0.0.1:50051"))
    );
    assert_eq!(
        value.get("session_id"),
        Some(&serde_json::json!("session-123"))
    );
    assert_eq!(
        value.get("client_callback_secret"),
        Some(&serde_json::json!("secret-123"))
    );
}

#[test]
fn finalize_request_callback_fields_default_when_absent() {
    let back: echo_agent_tools_api::FinalizeToolServerConfigRequest =
        serde_json::from_value(serde_json::json!({
            "tools": [],
            "system_reminders_enabled": false,
        }))
        .expect("deserialize sparse finalize request");
    assert_eq!(back.client_callback_addr, None);
    assert_eq!(back.session_id, None);
    assert_eq!(back.client_callback_secret, None);
}
