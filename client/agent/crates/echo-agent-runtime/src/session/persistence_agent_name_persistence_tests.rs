use super::*;

#[test]
fn summary_round_trips_agent_name_through_json() {
    let mut summary = Summary::new(
        &Info {
            id: acp::SessionId::new("test"),
            cwd: "/tmp".into(),
        },
        default_model_id(),
    )
    .unwrap();
    summary.agent_name = Some("cursor".into());

    let json = serde_json::to_string(&summary).unwrap();
    let deserialized: Summary = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.agent_name.as_deref(), Some("cursor"));
}

#[test]
fn summary_deserializes_without_agent_name_backward_compat() {
    // Simulate an old summary.json that lacks agent_name — must still
    // deserialize successfully (serde default → None).
    let json = r#"{
            "info": { "id": "old-session", "cwd": "/tmp" },
            "session_summary": "",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "num_messages": 0,
            "num_chat_messages": 0,
            "current_model_id": "test-model"
        }"#;
    let summary: Summary = serde_json::from_str(json).unwrap();
    assert!(
        summary.agent_name.is_none(),
        "old summaries without agent_name should deserialize as None"
    );
}

#[test]
fn summary_skips_none_agent_name_in_serialized_json() {
    let summary = Summary::new(
        &Info {
            id: acp::SessionId::new("test"),
            cwd: "/tmp".into(),
        },
        default_model_id(),
    )
    .unwrap();
    let json = serde_json::to_string(&summary).unwrap();
    assert!(
        !json.contains("agent_name"),
        "None agent_name should not appear in serialized JSON"
    );
}

#[test]
fn summary_includes_agent_name_when_set() {
    let mut summary = Summary::new(
        &Info {
            id: acp::SessionId::new("test"),
            cwd: "/tmp".into(),
        },
        default_model_id(),
    )
    .unwrap();
    summary.agent_name = Some("cursor".into());
    let json = serde_json::to_string(&summary).unwrap();
    assert!(json.contains("agent_name"));
    assert!(json.contains("cursor"));
}

#[test]
fn summary_round_trips_various_agent_names() {
    for name in [
        "cursor",
        "echo-agent-build",
        "echo-agent-build-plan",
        "codex",
        "browser-use",
    ] {
        let mut summary = Summary::new(
            &Info {
                id: acp::SessionId::new("test"),
                cwd: "/tmp".into(),
            },
            default_model_id(),
        )
        .unwrap();
        summary.agent_name = Some(name.into());

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: Summary = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.agent_name.as_deref(),
            Some(name),
            "round-trip failed for agent_name={name}"
        );
    }
}

#[test]
fn summary_with_agent_name_in_full_json() {
    // Verify agent_name deserializes correctly alongside all other fields.
    let json = r#"{
            "info": { "id": "full-session", "cwd": "/tmp" },
            "session_summary": "test session",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "num_messages": 10,
            "num_chat_messages": 5,
            "current_model_id": "cursor-model",
            "agent_name": "cursor",
            "generated_title": "Fix editor mode",
            "head_branch": "main"
        }"#;
    let summary: Summary = serde_json::from_str(json).unwrap();
    assert_eq!(summary.agent_name.as_deref(), Some("cursor"));
    assert_eq!(summary.current_model_id.0.as_ref(), "cursor-model");
    assert_eq!(summary.generated_title.as_deref(), Some("Fix editor mode"));
}
