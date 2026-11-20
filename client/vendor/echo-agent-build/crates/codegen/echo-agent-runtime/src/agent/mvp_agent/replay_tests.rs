//! Replay of records earlier builds wrote, asserted on what reaches the client.

use agent_client_protocol as acp;
use serde_json::Value;

use super::MvpAgent;
use super::replay::ReplayOrigin;
use crate::tools::task_completed_frame::FRAME_MAX_BYTES;
use echo_agent_acp::{AcpAgentGatewaySender as GatewaySender, AcpClientMessage};

/// The record is built from the real notification type, so renaming a field
/// the refit looks up by name fails this test rather than silently putting
/// oversized lines back on the wire.
#[tokio::test]
async fn replay_shrinks_an_oversized_completion() {
    let (agent, mut rx) = build_agent_with_gateway();
    let line = replay_line(&recorded_completion("Z".repeat(2 * 1024 * 1024)));

    agent.forward_raw_replay_line(
        &line,
        /*persist_data*/ None,
        /*target_client_id*/ None,
        ReplayOrigin::Live,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );

    let params = next_ext_notification_params(&mut rx).expect("the record must still be sent");
    assert!(
        params.len() <= FRAME_MAX_BYTES,
        "replayed {} bytes",
        params.len()
    );
    assert!(
        params.contains("/tmp/bg-old.log"),
        "the log pointer is kept"
    );
    assert!(
        params.contains("keep me"),
        "a field this build does not model must survive the refit"
    );
}

fn recorded_completion(output: String) -> Value {
    use crate::extensions::notification::{SessionNotification, SessionUpdate};

    let notification = SessionNotification {
        session_id: acp::SessionId::new("s"),
        update: SessionUpdate::TaskCompleted {
            task_snapshot: echo_agent_tools::types::TaskSnapshot {
                task_id: "bg-old".to_string(),
                command: "grep -r pattern .".to_string(),
                display_command: None,
                cwd: "/workspace".to_string(),
                start_time: std::time::SystemTime::now(),
                end_time: Some(std::time::SystemTime::now()),
                output,
                output_file: std::path::PathBuf::from("/tmp/bg-old.log"),
                truncated: false,
                output_total_bytes: 0,
                exit_code: Some(0),
                signal: None,
                completed: true,
                block_waited: false,
                explicitly_killed: false,
                kill_result_delivered: false,
                kind: Default::default(),
                owner_session_id: None,
                description: None,
                is_backgrounded: true,
            },
            will_wake: false,
        },
        meta: None,
    };
    let mut record = serde_json::to_value(&notification).expect("serialize");
    record["update"]["task_snapshot"]["a_field_from_another_build"] =
        Value::String("keep me".to_string());
    record
}

fn replay_line(record: &Value) -> String {
    serde_json::json!({ "method": "_echo.agent/session/update", "params": record }).to_string()
}

/// The branch a plain resume takes: `_meta` is added to the record, so the fit
/// has to happen after that and not before.
#[tokio::test]
async fn a_marked_replay_is_fitted_after_its_metadata_is_added() {
    let (agent, mut rx) = build_agent_with_gateway();
    let line = replay_line(&recorded_completion("Z".repeat(2 * 1024 * 1024)));
    let persist = serde_json::json!({ "padding": "p".repeat(8 * 1024) });

    agent.forward_raw_replay_line(
        &line,
        Some(&persist),
        /*target_client_id*/ None,
        ReplayOrigin::History,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );

    let params = next_ext_notification_params(&mut rx).expect("the record must still be sent");
    assert!(
        params.len() <= FRAME_MAX_BYTES,
        "replayed {} bytes once the metadata was added",
        params.len()
    );
    assert!(params.contains("isReplay"));
}

#[tokio::test]
async fn desktop_cursor_replay_preserves_event_id_and_stays_historical() {
    let (agent, mut rx) = build_agent_with_gateway();
    let mut record = recorded_completion(String::new());
    record["_meta"] = serde_json::json!({"eventId": "s-42"});
    agent.forward_raw_replay_line(
        &replay_line(&record),
        None,
        None,
        ReplayOrigin::DesktopCatchup,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );
    let params = next_ext_notification_params(&mut rx).unwrap();
    let value: Value = serde_json::from_str(&params).unwrap();
    assert_eq!(value["_meta"]["eventId"], "s-42");
    assert_eq!(value["_meta"]["isReplay"], true);
    assert_eq!(value["_meta"]["desktopCatchup"], true);
}

/// A completion nothing can shrink is dropped, because sending it is what
/// closes the connection.
#[tokio::test]
async fn replay_drops_a_completion_nothing_can_shrink() {
    let (agent, mut rx) = build_agent_with_gateway();
    let mut record = recorded_completion(String::new());
    record["update"]["task_snapshot"]["task_id"] = Value::String("t".repeat(2 * FRAME_MAX_BYTES));
    let line = replay_line(&record);

    agent.forward_raw_replay_line(
        &line,
        /*persist_data*/ None,
        /*target_client_id*/ None,
        ReplayOrigin::Live,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );

    assert!(next_ext_notification_params(&mut rx).is_none());
}

#[tokio::test]
async fn replay_forwards_records_within_the_limit_untouched() {
    let (agent, mut rx) = build_agent_with_gateway();
    let record = recorded_completion("hi\n".to_string());
    let line = replay_line(&record);

    agent.forward_raw_replay_line(
        &line,
        /*persist_data*/ None,
        /*target_client_id*/ None,
        ReplayOrigin::Live,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );

    let params = next_ext_notification_params(&mut rx).expect("forwarded");
    assert_eq!(params, serde_json::to_string(&record).unwrap());
}

#[tokio::test]
async fn replay_leaves_other_oversized_records_alone() {
    let (agent, mut rx) = build_agent_with_gateway();

    let recorded = format!(
        r#"{{"sessionId":"s","update":{{"sessionUpdate":"subagent_spawned","subagent_id":"{}"}}}}"#,
        "x".repeat(64 * 1024)
    );
    let line = format!(r#"{{"method":"_echo.agent/session/update","params":{recorded}}}"#);
    agent.forward_raw_replay_line(
        &line,
        /*persist_data*/ None,
        /*target_client_id*/ None,
        ReplayOrigin::Live,
        &mut crate::session::storage::ReplayToolCollapser::new(),
    );

    let params = next_ext_notification_params(&mut rx).expect("forwarded");
    assert_eq!(params, recorded);
}

#[tokio::test]
async fn parent_replay_does_not_emit_meta_less_unfinished_tool_call() {
    let (agent, mut rx) = build_agent_with_gateway();
    let mut collapser = crate::session::storage::ReplayToolCollapser::new();
    let tool = r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"bash","status":"pending"},"_meta":{"eventId":"s-1"}}}"#;
    let start = r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","title":"ls"},"_meta":{"eventId":"s-2"}}}"#;
    agent.forward_raw_replay_line(tool, None, None, ReplayOrigin::Live, &mut collapser);
    agent.forward_raw_replay_line(start, None, None, ReplayOrigin::Live, &mut collapser);
    assert_eq!(collapser.pending_len(), 1);
    assert!(
        rx.try_recv().is_err(),
        "parent must not synthesize a meta-less ToolCall at EOF"
    );
}

/// Stale-task reconciliation on a cold load builds its completion from a
/// recorded command of any size, so its line is measured like the rest.
#[tokio::test]
async fn a_stale_task_completion_is_frame_bounded() {
    let (agent, mut rx) = build_agent_with_gateway();
    let dir = tempfile::tempdir().unwrap();
    let line = format!(
        r#"{{"timestamp":1,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"task_backgrounded","task_id":"stale-1","command":"{}","cwd":"/tmp"}}}}}}"#,
        "c".repeat(64 * 1024)
    );
    let path = dir.path().join("updates.jsonl");
    std::fs::write(&path, line).unwrap();

    agent.reconcile_stale_background_tasks(&acp::SessionId::new("s"), &Some(path));

    let params = next_ext_notification_params(&mut rx).expect("the stale task must be reported");
    assert!(
        params.len() <= FRAME_MAX_BYTES,
        "reconciliation emitted {} bytes",
        params.len()
    );
    assert!(params.contains("session_restart"));
}

fn build_agent_with_gateway() -> (
    MvpAgent,
    tokio::sync::mpsc::UnboundedReceiver<AcpClientMessage>,
) {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};

    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager = std::sync::Arc::new(AuthManager::new(
        temp_dir.path(),
        EchoAgentComConfig::default(),
    ));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let agent = MvpAgent::new(
        GatewaySender::new(tx),
        &AgentConfig::default(),
        auth_manager,
        None,
    )
    .expect("valid test config");
    (agent, rx)
}

fn next_ext_notification_params(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AcpClientMessage>,
) -> Option<String> {
    let mut params = None;
    while let Ok(msg) = rx.try_recv() {
        if let AcpClientMessage::ExtNotification(args) = msg {
            params.get_or_insert_with(|| args.request.params.get().to_string());
            let _ = args.response_tx.send(Ok(()));
        }
    }
    params
}
