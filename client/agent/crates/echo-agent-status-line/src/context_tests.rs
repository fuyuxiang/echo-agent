use serde_json::json;

use super::*;

const DIR: &str = "/home/user/project";

/// The command-row stdin shape the pager's guide test also reads, so
/// a field renamed here fails in front of all three. It carries `session_name`,
/// which the client overlays for a command row; the agent's own notification
/// leaves that field null.
fn wire_fixture() -> serde_json::Value {
    let mut fixture: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/status_wire.json"))
            .expect("the shared fixture must be valid JSON");
    fixture
        .as_object_mut()
        .expect("the fixture is an object")
        .remove("_comment");
    fixture
}

#[test]
fn every_field_survives_a_round_trip_through_the_shared_fixture() {
    // Every field, rather than `..Default::default()`: a new one is a compile
    // error here, and then a missing name in the fixture the SDKs read.
    let ctx = StatusLineContext {
        schema_version: Some(STATUS_LINE_SCHEMA_VERSION),
        cwd: DIR.into(),
        session_id: Some("019fa651-6d59-7c83-a4f3-5a391e6901a1".into()),
        session_name: Some("add status line".into()),
        prompt_id: Some("97135ed2-71a5-4581-b959-3341bbd03e5f".into()),
        transcript_path: Some("/home/user/sessions/019fa651/updates.jsonl".into()),
        model: StatusLineModel {
            id: Some("echo-agent-4.5".into()),
            display_name: Some("EchoAgent 4.5".into()),
        },
        workspace: StatusLineWorkspace {
            current_dir: DIR.into(),
            repo_root: Some(DIR.into()),
            branch: Some("main".into()),
            git_worktree: Some("feature-x".into()),
            repo: Some(StatusLineRepo {
                host: "github.com".into(),
                owner: Some("owner".into()),
                name: "repo".into(),
            }),
        },
        version: "0.2.112".to_string(),
        cost: StatusLineCost {
            total_cost_usd: Some(0.0123),
            total_duration_ms: 45_000,
            total_api_duration_ms: Some(2_300),
        },
        context_window: StatusLineContextWindow {
            context_window_size: Some(500_000),
            context_tokens: Some(40_000),
            session_input_tokens: Some(52_000),
            session_output_tokens: Some(9_500),
            session_usage: Some(StatusLineSessionUsage {
                input_tokens: 10_000,
                output_tokens: 9_500,
                cache_creation_input_tokens: 2_000,
                cache_read_input_tokens: 40_000,
            }),
            used_percentage: Some(8),
            remaining_percentage: Some(92),
            auto_compact_threshold_percent: Some(80),
        },
        effort: Some(StatusLineEffort {
            level: "high".into(),
        }),
        turn: Some(StatusLineTurn {
            started_at_ms: 1_730_000_000_000,
        }),
        worktree: Some(StatusLineWorktree {
            name: Some("feature-x".into()),
            path: "/home/user/wt/feature-x".into(),
            branch: Some("feature-x".into()),
            main_worktree_root: Some(DIR.into()),
        }),
        trigger: Some(StatusLineTrigger::RefreshInterval),
    };

    assert_eq!(
        serde_json::to_value(&ctx).unwrap(),
        wire_fixture(),
        "the type and the fixture have drifted; update both SDK suites with it"
    );
    let parsed: StatusLineContext =
        serde_json::from_value(wire_fixture()).expect("the wire shape must parse back");
    assert_eq!(parsed, ctx, "a name the type writes but cannot read back");

    assert_eq!(
        serde_json::to_value(StatusLineTrigger::RefreshInterval).unwrap(),
        json!("refresh_interval")
    );
    assert_eq!(
        serde_json::to_value(StatusLineTrigger::State).unwrap(),
        json!("state")
    );
}

#[test]
fn unknown_data_is_omitted_rather_than_faked() {
    let mut bare = StatusLineContext::default();
    bare.workspace.repo = Some(StatusLineRepo {
        host: "example.com".into(),
        owner: None,
        name: "widget".into(),
    });

    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({
            "schema_version": 1,
            "cwd": "", "version": "",
            "model": {},
            "workspace": {
                "current_dir": "",
                "repo": { "host": "example.com", "name": "widget" },
            },
            "cost": { "total_duration_ms": 0 },
            "context_window": {},
        }),
        "what EchoAgent cannot source is omitted; a context window reported as 0 \
         would paint `0% ctx` over a full one"
    );
}

#[test]
fn payload_missing_newer_fields_still_deserializes() {
    let minimal: StatusLineContext = serde_json::from_str(r#"{"cwd":"/tmp"}"#).unwrap();
    assert_eq!(
        minimal.schema_version, None,
        "absent means the sender predates the field"
    );
    assert!(minimal.context_window.context_window_size.is_none());
}
