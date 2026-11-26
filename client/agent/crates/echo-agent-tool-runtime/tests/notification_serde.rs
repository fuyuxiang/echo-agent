//! Round-trip every `ToolNotification` variant through serde_json and
//! assert the wire shape is what consumers expect.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use echo_agent_tool_runtime::{
    BashExecutionBackgrounded, BashExecutionComplete, BashExecutionFailed, BashExecutionTimeout,
    BashNotificationBase, BashOutputChunk, FileWritten, LspServerCrashed, LspServerFailed,
    LspServerReady, LspServerRetrying, LspServerStarting, MonitorEvent, PlanModeEntered,
    PlanModeExited, ScheduledTaskCreated, ScheduledTaskFired, ScheduledTaskRemoved, TaskKind,
    TaskSnapshot, ToolNotification, UserQuestionAsked,
};

fn base() -> BashNotificationBase {
    BashNotificationBase {
        tool_call_id: "call-1".into(),
        command: "echo hi".into(),
        output: b"hi\n".to_vec(),
        total_bytes: 3,
        truncated: false,
        cwd: PathBuf::from("/tmp"),
    }
}

fn round_trip(value: &ToolNotification) -> Value {
    let json = serde_json::to_value(value).expect("serialize");
    let back: ToolNotification = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(*value, back, "round-trip must match");
    json
}

fn assert_type_tag(json: &Value, expected: &str) {
    assert_eq!(json["type"], json!(expected), "wire type tag mismatch");
}

#[test]
fn bash_output_chunk_round_trip() {
    let n = ToolNotification::BashOutputChunk(BashOutputChunk { base: base() });
    let json = round_trip(&n);
    assert_type_tag(&json, "BashOutputChunk");
    assert_eq!(json["command"], json!("echo hi"));
}

#[test]
fn bash_execution_complete_round_trip() {
    let n = ToolNotification::BashExecutionComplete(BashExecutionComplete {
        base: base(),
        exit_code: Some(0),
        signal: None,
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "BashExecutionComplete");
    assert_eq!(json["exit_code"], json!(0));
}

#[test]
fn bash_execution_complete_was_signaled_helper() {
    let none = BashExecutionComplete {
        base: base(),
        exit_code: Some(1),
        signal: None,
    };
    assert!(!none.was_signaled());
    let killed = BashExecutionComplete {
        base: base(),
        exit_code: None,
        signal: Some("SIGKILL".into()),
    };
    assert!(killed.was_signaled());
}

#[test]
fn bash_execution_timeout_round_trip() {
    let n = ToolNotification::BashExecutionTimeout(BashExecutionTimeout {
        base: base(),
        elapsed: Duration::from_secs(30),
        timeout: Duration::from_secs(20),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "BashExecutionTimeout");
}

#[test]
fn bash_execution_backgrounded_round_trip() {
    let n = ToolNotification::BashExecutionBackgrounded(BashExecutionBackgrounded {
        base: base(),
        output_file: PathBuf::from("/tmp/out.log"),
        task_id: "bg-1".into(),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "BashExecutionBackgrounded");
    assert_eq!(json["task_id"], json!("bg-1"));
}

#[test]
fn bash_execution_failed_round_trip() {
    let n = ToolNotification::BashExecutionFailed(BashExecutionFailed {
        tool_call_id: "call-2".into(),
        command: "missing".into(),
        cwd: PathBuf::from("/tmp"),
        error: "not found".into(),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "BashExecutionFailed");
}

#[test]
fn file_written_round_trip_includes_previous_content() {
    let n = ToolNotification::FileWritten(FileWritten {
        tool_call_id: "call-3".into(),
        absolute_path: PathBuf::from("/tmp/x"),
        content: "after".into(),
        previous_content: Some("before".into()),
        is_new_file: false,
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "FileWritten");
    assert_eq!(json["previous_content"], json!("before"));
}

#[test]
fn task_completed_round_trip() {
    let snap = TaskSnapshot {
        task_id: "t-1".into(),
        command: "echo".into(),
        display_command: None,
        cwd: "/tmp".into(),
        start_time: SystemTime::UNIX_EPOCH,
        end_time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        output: "out".into(),
        output_file: PathBuf::from("/tmp/out"),
        truncated: false,
        exit_code: Some(0),
        signal: None,
        completed: true,
        kind: TaskKind::Bash,
        output_total_bytes: 0,
    };
    assert!((snap.duration_secs() - 1.0).abs() < 0.001);
    let n = ToolNotification::TaskCompleted(snap);
    let json = round_trip(&n);
    assert_type_tag(&json, "TaskCompleted");
}

#[test]
fn plan_mode_entered_round_trip() {
    let n = ToolNotification::PlanModeEntered(PlanModeEntered {
        tool_call_id: "call-4".into(),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "PlanModeEntered");
}

#[test]
fn plan_mode_exited_round_trip() {
    let n = ToolNotification::PlanModeExited(PlanModeExited {
        tool_call_id: "call-5".into(),
        plan_content: Some("plan".into()),
        plan_file_path: ".echo-agent/plan.md".into(),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "PlanModeExited");
    assert_eq!(json["plan_file_path"], json!(".echo-agent/plan.md"));
}

#[test]
fn user_question_asked_round_trip() {
    let n = ToolNotification::UserQuestionAsked(UserQuestionAsked {
        tool_call_id: "call-6".into(),
        questions_json: json!([{"q": "ok?"}]),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "UserQuestionAsked");
}

#[test]
fn lsp_lifecycle_variants_round_trip() {
    let variants = vec![
        ToolNotification::LspServerStarting(LspServerStarting {
            server_name: "rust".into(),
            command: "rust-analyzer".into(),
        }),
        ToolNotification::LspServerReady(LspServerReady {
            server_name: "rust".into(),
        }),
        ToolNotification::LspServerCrashed(LspServerCrashed {
            server_name: "rust".into(),
        }),
        ToolNotification::LspServerRetrying(LspServerRetrying {
            server_name: "rust".into(),
            attempt: 1,
            max_restarts: 3,
            backoff_ms: 500,
        }),
        ToolNotification::LspServerFailed(LspServerFailed {
            server_name: "rust".into(),
            error: "init failed".into(),
            attempts: 0,
        }),
    ];
    for v in &variants {
        round_trip(v);
    }
}

#[test]
fn scheduled_task_variants_round_trip() {
    let fired = ToolNotification::ScheduledTaskFired(ScheduledTaskFired {
        task_id: "s-1".into(),
        prompt: "do thing".into(),
        human_schedule: "every 5 minutes".into(),
        next_fire_at: Some("2025-01-01T00:00:00Z".into()),
    });
    round_trip(&fired);

    let removed = ToolNotification::ScheduledTaskRemoved(ScheduledTaskRemoved {
        task_id: "s-1".into(),
    });
    round_trip(&removed);

    let created = ToolNotification::ScheduledTaskCreated(ScheduledTaskCreated {
        task_id: "s-2".into(),
        prompt: "another".into(),
        human_schedule: "once".into(),
        next_fire_at: None,
    });
    round_trip(&created);
}

#[test]
fn monitor_event_round_trip() {
    let n = ToolNotification::MonitorEvent(MonitorEvent {
        task_id: "m-1".into(),
        description: "errors in deploy.log".into(),
        event_text: "<monitor-event>...</monitor-event>".into(),
        raw_text: "...".into(),
    });
    let json = round_trip(&n);
    assert_type_tag(&json, "MonitorEvent");
}

#[test]
fn task_kind_default_is_bash_and_round_trips() {
    assert_eq!(TaskKind::default(), TaskKind::Bash);
    let bash_json = serde_json::to_value(TaskKind::Bash).unwrap();
    let monitor_json = serde_json::to_value(TaskKind::Monitor).unwrap();
    assert_eq!(bash_json, json!("bash"));
    assert_eq!(monitor_json, json!("monitor"));
}

#[test]
fn variant_count_matches_variant_name() {
    let all_variants: Vec<ToolNotification> = vec![
        ToolNotification::BashOutputChunk(BashOutputChunk { base: base() }),
        ToolNotification::BashExecutionComplete(BashExecutionComplete {
            base: base(),
            exit_code: None,
            signal: None,
        }),
        ToolNotification::BashExecutionTimeout(BashExecutionTimeout {
            base: base(),
            elapsed: Duration::ZERO,
            timeout: Duration::ZERO,
        }),
        ToolNotification::BashExecutionBackgrounded(BashExecutionBackgrounded {
            base: base(),
            output_file: PathBuf::new(),
            task_id: String::new(),
        }),
        ToolNotification::BashExecutionFailed(BashExecutionFailed {
            tool_call_id: String::new(),
            command: String::new(),
            cwd: PathBuf::new(),
            error: String::new(),
        }),
        ToolNotification::FileWritten(FileWritten {
            tool_call_id: String::new(),
            absolute_path: PathBuf::new(),
            content: String::new(),
            previous_content: None,
            is_new_file: true,
        }),
        ToolNotification::TaskCompleted(TaskSnapshot {
            task_id: String::new(),
            command: String::new(),
            display_command: None,
            cwd: String::new(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: None,
            output: String::new(),
            output_file: PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: None,
            completed: false,
            kind: TaskKind::Bash,
            output_total_bytes: 0,
        }),
        ToolNotification::PlanModeEntered(PlanModeEntered {
            tool_call_id: String::new(),
        }),
        ToolNotification::PlanModeExited(PlanModeExited {
            tool_call_id: String::new(),
            plan_content: None,
            plan_file_path: String::new(),
        }),
        ToolNotification::UserQuestionAsked(UserQuestionAsked {
            tool_call_id: String::new(),
            questions_json: json!(null),
        }),
        ToolNotification::LspServerStarting(LspServerStarting {
            server_name: String::new(),
            command: String::new(),
        }),
        ToolNotification::LspServerReady(LspServerReady {
            server_name: String::new(),
        }),
        ToolNotification::LspServerCrashed(LspServerCrashed {
            server_name: String::new(),
        }),
        ToolNotification::LspServerRetrying(LspServerRetrying {
            server_name: String::new(),
            attempt: 0,
            max_restarts: 0,
            backoff_ms: 0,
        }),
        ToolNotification::LspServerFailed(LspServerFailed {
            server_name: String::new(),
            error: String::new(),
            attempts: 0,
        }),
        ToolNotification::ScheduledTaskFired(ScheduledTaskFired {
            task_id: String::new(),
            prompt: String::new(),
            human_schedule: String::new(),
            next_fire_at: None,
        }),
        ToolNotification::ScheduledTaskRemoved(ScheduledTaskRemoved {
            task_id: String::new(),
        }),
        ToolNotification::ScheduledTaskCreated(ScheduledTaskCreated {
            task_id: String::new(),
            prompt: String::new(),
            human_schedule: String::new(),
            next_fire_at: None,
        }),
        ToolNotification::MonitorEvent(MonitorEvent {
            task_id: String::new(),
            description: String::new(),
            event_text: String::new(),
            raw_text: String::new(),
        }),
    ];
    let names: std::collections::HashSet<_> =
        all_variants.iter().map(|n| n.variant_name()).collect();
    assert_eq!(
        names.len(),
        19,
        "expected 19 distinct variant names; if you added a notification, extend the test list and `variant_name`"
    );
    assert_eq!(all_variants.len(), 19);
}

#[test]
fn handle_send_helpers_round_trip_through_channel() {
    use echo_agent_tool_runtime::ToolNotificationHandle;
    use futures::stream::StreamExt;

    let (handle, mut rx) = ToolNotificationHandle::channel();
    handle.send_bash_output_chunk(BashOutputChunk { base: base() });
    handle.send_lsp_ready(LspServerReady {
        server_name: "rust".into(),
    });
    drop(handle);

    let mut received = Vec::new();
    futures::executor::block_on(async {
        while let Some(item) = rx.next().await {
            received.push(item.variant_name());
        }
    });
    assert_eq!(received, vec!["BashOutputChunk", "LspServerReady"]);
}

#[test]
fn noop_handle_does_not_panic_or_record() {
    let handle = echo_agent_tool_runtime::ToolNotificationHandle::noop();
    handle.send_bash_output_chunk(BashOutputChunk { base: base() });
    handle.send_lsp_ready(LspServerReady {
        server_name: "x".into(),
    });
    // No assertion needed — the handle drops sends silently.
}

#[test]
fn output_lossy_handles_invalid_utf8() {
    let mut b = base();
    b.output = vec![0xFF, b'a', b'b'];
    let cow = b.output_lossy();
    assert!(cow.contains("ab"));
    assert!(cow.contains('\u{FFFD}'));
}
