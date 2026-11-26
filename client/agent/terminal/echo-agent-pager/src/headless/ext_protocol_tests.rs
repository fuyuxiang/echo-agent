use super::*;
use crate::headless::reducer::StreamEvent;
use pretty_assertions::assert_eq;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture_logs(f: impl FnOnce()) -> String {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    logs.text()
}

fn make_ext_notif(
    method: &str,
    update: serde_json::Value,
) -> echo_agent_acp::AcpArgsBox<acp::ExtNotification> {
    make_raw_ext_notif(
        method,
        serde_json::json!({
            "sessionId": "sess-1",
            "update": update,
        }),
    )
}

fn make_raw_ext_notif(
    method: &str,
    params: serde_json::Value,
) -> echo_agent_acp::AcpArgsBox<acp::ExtNotification> {
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    echo_agent_acp::AcpArgs {
        request: acp::ExtNotification::new(method, raw.into()),
        response_tx: tx,
    }
    .boxed()
}

#[test]
fn headless_task_backgrounded_parses_task_id() {
    let notif = make_ext_notif(
        "echo.agent/task_backgrounded",
        serde_json::json!({
            "sessionUpdate": "task_backgrounded",
            "task_id": "task-abc",
        }),
    );
    assert!(matches!(
        handle_ext_notification(&notif),
        ExtEvent::TaskBackgrounded { task_id, is_monitor: false } if task_id == "task-abc"
    ));
}

#[test]
fn headless_task_backgrounded_numeric_task_id_is_coerced() {
    let notif = make_ext_notif(
        "echo.agent/task_backgrounded",
        serde_json::json!({
            "sessionUpdate": "task_backgrounded",
            "task_id": 4242,
        }),
    );
    assert!(matches!(
        handle_ext_notification(&notif),
        ExtEvent::TaskBackgrounded { task_id, is_monitor: false } if task_id == "4242"
    ));
}

#[test]
fn headless_task_completed_numeric_task_id_is_coerced() {
    let notif = make_ext_notif(
        "echo.agent/task_completed",
        serde_json::json!({
            "sessionUpdate": "task_completed",
            "task_snapshot": { "task_id": 4242 }
        }),
    );
    assert!(matches!(
        handle_ext_notification(&notif),
        ExtEvent::TaskCompleted { task_id } if task_id == "4242"
    ));
}

#[test]
fn headless_task_backgrounded_with_monitor_description_is_monitor() {
    let notif = make_ext_notif(
        "echo.agent/task_backgrounded",
        serde_json::json!({
            "sessionUpdate": "task_backgrounded",
            "task_id": "mon-1",
            "monitor_description": "watching logs",
        }),
    );
    assert!(matches!(
        handle_ext_notification(&notif),
        ExtEvent::TaskBackgrounded { task_id, is_monitor: true } if task_id == "mon-1"
    ));
}

#[test]
fn headless_task_completed_parses_task_id() {
    let notif = make_ext_notif(
        "echo.agent/task_completed",
        serde_json::json!({
            "sessionUpdate": "task_completed",
            "task_snapshot": { "task_id": "task-abc" }
        }),
    );
    assert!(matches!(
        handle_ext_notification(&notif),
        ExtEvent::TaskCompleted { task_id } if task_id == "task-abc"
    ));
}

#[test]
fn headless_subagent_spawned_and_finished_parse() {
    let spawned = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "subagent_spawned",
            "subagent_id": "sub-1",
            "parent_session_id": "p",
            "child_session_id": "c",
            "subagent_type": "explore",
            "description": "test"
        }),
    );
    assert!(matches!(
        handle_ext_notification(&spawned),
        ExtEvent::SubagentSpawned { subagent_id } if subagent_id == "sub-1"
    ));
    let finished = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "subagent_finished",
            "subagent_id": "sub-1",
            "child_session_id": "c",
            "status": "completed",
            "tool_calls": 0,
            "turns": 1,
            "duration_ms": 5
        }),
    );
    assert!(matches!(
        handle_ext_notification(&finished),
        ExtEvent::SubagentFinished { subagent_id } if subagent_id == "sub-1"
    ));
}

#[test]
fn headless_response_completed_parses_per_response_fields() {
    let notif = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "response_completed",
            "message_id": "msg_01",
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4,
                "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 0,
            },
            "signature": "sig-xyz",
            "stop_sequence": "<END>",
        }),
    );
    let ExtEvent::Stream(event) = handle_ext_notification(&notif) else {
        panic!("expected Stream event");
    };
    let StreamEvent::ResponseCompleted {
        message_id,
        stop_reason,
        usage,
        signature,
        stop_sequence,
    } = *event
    else {
        panic!("expected ResponseCompleted");
    };
    assert_eq!(message_id.as_deref(), Some("msg_01"));
    assert_eq!(stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(signature.as_deref(), Some("sig-xyz"));
    assert_eq!(stop_sequence.as_deref(), Some("<END>"));
    let usage = usage.expect("usage present");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cache_read_input_tokens, 2);
}

#[test]
fn headless_response_started_parses_per_response_fields() {
    let notif = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "response_started",
            "message_id": "msg_01",
            "model": "echo-agent-4",
            "input_tokens": 42,
            "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 3,
        }),
    );
    let ExtEvent::Stream(event) = handle_ext_notification(&notif) else {
        panic!("expected Stream event");
    };
    let StreamEvent::ResponseStarted {
        message_id,
        model,
        input_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    } = *event
    else {
        panic!("expected ResponseStarted");
    };
    assert_eq!(message_id.as_deref(), Some("msg_01"));
    assert_eq!(model.as_deref(), Some("echo-agent-4"));
    assert_eq!(input_tokens, 42);
    assert_eq!(cache_read_input_tokens, 7);
    assert_eq!(cache_creation_input_tokens, 3);
}

#[test]
fn headless_reasoning_completed_parses_signature() {
    let notif = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "reasoning_completed",
            "signature": "sig-xyz",
        }),
    );
    let ExtEvent::Stream(event) = handle_ext_notification(&notif) else {
        panic!("expected Stream event");
    };
    let StreamEvent::ReasoningCompleted { signature } = *event else {
        panic!("expected ReasoningCompleted");
    };
    assert_eq!(signature.as_deref(), Some("sig-xyz"));
}

#[test]
fn headless_undecodable_known_background_task_errors_not_silent() {
    let notif = make_ext_notif(
        "echo.agent/task_backgrounded",
        serde_json::json!({
            "sessionUpdate": "task_backgrounded",
            "task_id": { "nested": "object" },
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "undecodable known method degrades to None");
    assert!(
        logs.contains("task_backgrounded"),
        "log names the method: {logs}"
    );
    assert!(logs.contains("ERROR"), "logged at error level: {logs}");
}

#[test]
fn headless_task_backgrounded_mismatched_tag_errors_not_silent() {
    let notif = make_ext_notif(
        "echo.agent/task_backgrounded",
        serde_json::json!({
            "sessionUpdate": "task_completed",
            "task_id": "task-abc",
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "mismatched-tag known method degrades to None");
    assert!(
        logs.contains("task_backgrounded"),
        "log names the method: {logs}"
    );
    assert!(logs.contains("ERROR"), "logged at error level: {logs}");
}

#[test]
fn headless_task_completed_mismatched_tag_errors_not_silent() {
    let notif = make_ext_notif(
        "echo.agent/task_completed",
        serde_json::json!({
            "sessionUpdate": "task_backgrounded",
            "task_snapshot": { "task_id": "task-abc" },
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "mismatched-tag known method degrades to None");
    assert!(
        logs.contains("task_completed"),
        "log names the method: {logs}"
    );
    assert!(logs.contains("ERROR"), "logged at error level: {logs}");
}

#[test]
fn headless_malformed_known_response_boundary_warns_not_silent() {
    let notif = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({
            "sessionUpdate": "response_completed",
            "usage": "not-an-object",
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "malformed known notification degrades to None");
    assert!(
        logs.contains("session notification"),
        "warning describes the malformed session notification: {logs}"
    );
    assert!(logs.contains("WARN"), "logged at warn level: {logs}");
}

#[test]
fn headless_session_update_unknown_method_is_none() {
    let payload = serde_json::json!({
        "sessionId": "sess-1",
        "update": {
            "sessionUpdate": "subagent_spawned",
            "subagent_id": "sub-1"
        }
    });
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let notif = echo_agent_acp::AcpArgs {
        request: acp::ExtNotification::new("echo.agent/other", raw.into()),
        response_tx: tx,
    }
    .boxed();
    assert!(matches!(handle_ext_notification(&notif), ExtEvent::None));
}

#[test]
fn headless_session_notification_task_tag_errors_not_silent() {
    for tag in ["task_backgrounded", "task_completed"] {
        let notif = make_ext_notif(
            "echo.agent/session_notification",
            serde_json::json!({
                "sessionUpdate": tag,
                "task_id": "task-abc",
            }),
        );
        let mut is_none = false;
        let logs = capture_logs(|| {
            is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
        });
        assert!(is_none, "misrouted lifecycle tag degrades to None ({tag})");
        assert!(logs.contains(tag), "log names the tag {tag}: {logs}");
        assert!(
            logs.contains("ERROR"),
            "logged at error level ({tag}): {logs}"
        );
    }
}

#[test]
fn headless_version_mismatch_logs_warn_with_both_versions() {
    let notif = make_raw_ext_notif(
        "echo.agent/leader/version_mismatch",
        serde_json::json!({
            "clientVersion": "0.1.157",
            "leaderVersion": "0.1.150",
            "message": "Client version 0.1.157 differs from leader version 0.1.150.",
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "version mismatch is log-only in headless");
    assert!(logs.contains("WARN"), "logged at warn level: {logs}");
    assert!(
        logs.contains("version_mismatch"),
        "log names the method: {logs}"
    );
    let banner = crate::glyphs::sanitize_toast_message(
        "⚠ Version mismatch: client 0.1.157, leader 0.1.150. Restart echoagent to match",
    );
    assert!(
        logs.contains(banner.as_ref()),
        "log carries the exact banner: {logs}"
    );
}

#[test]
fn headless_version_mismatch_without_message_still_warns() {
    let notif = make_raw_ext_notif(
        "echo.agent/leader/version_mismatch",
        serde_json::json!({
            "clientVersion": "0.1.157",
            "leaderVersion": "0.1.150",
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none);
    assert!(logs.contains("WARN"), "logged at warn level: {logs}");
    let banner = crate::glyphs::sanitize_toast_message(
        "⚠ Version mismatch: client 0.1.157, leader 0.1.150. Restart echoagent to match",
    );
    assert!(
        logs.contains(banner.as_ref()),
        "missing message must still log both versions: {logs}"
    );
}

#[test]
fn headless_version_mismatch_malformed_warns_distinctly() {
    for params in [
        serde_json::json!({}),
        serde_json::json!({ "message": "only a message" }),
        serde_json::Value::String("not-an-object".into()),
    ] {
        let desc = params.to_string();
        let notif = make_raw_ext_notif("echo.agent/leader/version_mismatch", params);
        let logs = capture_logs(|| {
            assert!(
                matches!(handle_ext_notification(&notif), ExtEvent::None),
                "malformed params must not crash: {desc}"
            );
        });
        assert!(logs.contains("WARN"), "parse failure must warn: {logs}");
        assert!(
            logs.contains("version_mismatch"),
            "parse failure must name the method: {logs}"
        );
        assert!(
            !logs.contains("⚠ Version mismatch: client"),
            "malformed payload must not emit the success banner: {logs}"
        );
    }
}

#[test]
fn headless_unknown_leader_method_is_silent_none() {
    let notif = make_raw_ext_notif(
        "echo.agent/leader/not_a_method",
        serde_json::json!({
            "clientVersion": "0.1.157",
            "leaderVersion": "0.1.150",
        }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none);
    assert!(
        !logs.contains("WARN"),
        "unknown method must stay a clean ignore: {logs}"
    );
}

#[test]
fn headless_session_notification_unknown_tag_is_clean_ignore() {
    let notif = make_ext_notif(
        "echo.agent/session_notification",
        serde_json::json!({ "sessionUpdate": "totally_unknown_display_tag" }),
    );
    let mut is_none = false;
    let logs = capture_logs(|| {
        is_none = matches!(handle_ext_notification(&notif), ExtEvent::None);
    });
    assert!(is_none, "unknown display tag degrades to None");
    assert!(
        !logs.contains("ERROR"),
        "an unknown display tag is a clean ignore, not an error: {logs}"
    );
}

/// Call `reply_headless_ext_method` and return what landed on the oneshot.
fn ext_method_reply(
    method: &str,
    params: serde_json::Value,
) -> echo_agent_acp::AcpResult<acp::ExtResponse> {
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    reply_headless_ext_method(
        echo_agent_acp::AcpArgs {
            request: acp::ExtRequest::new(method, raw.into()),
            response_tx: tx,
        }
        .boxed(),
    );
    rx.try_recv()
        .expect("ext_method must be answered, never dropped")
}

/// `echo.agent/ask_user_question` gets a typed `cancelled` reply on the wire;
/// malformed params are still answered (known methods do not parse params).
#[test]
fn mcp_elicit_replies_cancelled() {
    use echo_agent_tools::mcp_elicitation::McpElicitExtResponse;
    let raw = ext_method_reply("echo.agent/mcp/elicit", serde_json::json!({}))
        .expect("policy reply, not an error");
    let typed: McpElicitExtResponse = serde_json::from_str(raw.0.get()).expect("typed cancel");
    assert!(matches!(typed, McpElicitExtResponse::Cancel));
}

#[test]
fn ask_user_question_replies_cancelled() {
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::AskUserQuestionExtResponse;
    for params in [
        serde_json::json!({
            "sessionId": "s", "toolCallId": "t", "questions": [], "mode": "default",
        }),
        serde_json::json!("not-an-object"),
    ] {
        let resp = ext_method_reply("echo.agent/ask_user_question", params)
            .expect("policy reply, not an error");
        let parsed: AskUserQuestionExtResponse = serde_json::from_str(resp.0.get())
            .expect("wire reply must deserialize as the typed response");
        assert!(matches!(parsed, AskUserQuestionExtResponse::Cancelled));
    }
}

/// `echo.agent/exit_plan_mode` is approved (no feedback) so the shell executes the
/// exit and the model proceeds to implement.
#[test]
fn exit_plan_mode_replies_approved() {
    use echo_agent_tools::implementations::echo_agent_build::exit_plan_mode::ExitPlanModeExtResponse;
    let resp = ext_method_reply(
        "echo.agent/exit_plan_mode",
        serde_json::json!({"sessionId": "s", "toolCallId": "t"}),
    )
    .expect("policy reply, not an error");
    let parsed: ExitPlanModeExtResponse = serde_json::from_str(resp.0.get())
        .expect("wire reply must deserialize as the typed response");
    assert_eq!(parsed.outcome, "approved");
    assert!(parsed.feedback.is_none());
}

/// Unknown methods (including lookalikes of the known ones) get a
/// MethodNotFound error carrying the method name — never a dropped channel.
#[test]
fn unknown_ext_method_replies_method_not_found() {
    for method in [
        "echo.agent/some_future_method",
        "echo.agent/ask_user_questions",
        "echo.agent/exit_plan_mode2",
        "ask_user_question",
    ] {
        let err = ext_method_reply(method, serde_json::json!({}))
            .expect_err("unknown method must be an error reply");
        assert_eq!(i32::from(err.code), -32601, "method={method}");
        assert!(
            err.message.contains(method),
            "error must carry the method name: {method} -> {}",
            err.message
        );
    }
}

/// A receiver dropped before the reply must not panic the responder.
#[test]
fn dropped_receiver_does_not_panic() {
    let raw = serde_json::value::to_raw_value(&serde_json::json!({})).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    drop(rx);
    reply_headless_ext_method(
        echo_agent_acp::AcpArgs {
            request: acp::ExtRequest::new("echo.agent/ask_user_question", raw.into()),
            response_tx: tx,
        }
        .boxed(),
    );
}
