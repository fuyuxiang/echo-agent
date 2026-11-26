use std::path::PathBuf;
use std::time::SystemTime;

use agent_client_protocol as acp;
use echo_agent_tools::computer::types::TaskKind;
use pretty_assertions::assert_eq;

use super::*;

fn notification(output: &str) -> SessionNotification {
    SessionNotification {
        session_id: acp::SessionId::new("test-session"),
        update: SessionUpdate::TaskCompleted {
            task_snapshot: TaskSnapshot {
                task_id: "bg-1".to_string(),
                command: "grep -r pattern .".to_string(),
                display_command: None,
                cwd: "/workspace".to_string(),
                start_time: SystemTime::now(),
                end_time: Some(SystemTime::now()),
                output: output.to_string(),
                output_file: PathBuf::from("/tmp/bg-1.log"),
                truncated: false,
                output_total_bytes: output.len(),
                exit_code: Some(0),
                signal: None,
                completed: true,
                block_waited: false,
                explicitly_killed: false,
                kill_result_delivered: false,
                kind: TaskKind::Bash,
                owner_session_id: None,
                description: None,
                is_backgrounded: true,
            },
            will_wake: false,
        },
        meta: None,
    }
}

/// The full line a client reads: the body, its wrapper, and the newline.
fn frame_len(params: &RawValue) -> usize {
    WRAPPER_BYTES + METHOD.len() + params.get().len()
}

#[test]
fn small_output_is_untouched() {
    let mut notification = notification("all done\n");
    let params = encode(&mut notification).unwrap();

    assert!(frame_len(&params) <= FRAME_MAX_BYTES);
    let snapshot = task_snapshot(&mut notification).unwrap();
    assert_eq!(snapshot.output, "all done\n");
    assert!(!snapshot.truncated);
}

#[test]
fn a_multi_megabyte_log_fits_and_points_at_the_file() {
    let mut notification = notification(&"Z".repeat(2 * 1024 * 1024));
    let params = encode(&mut notification).unwrap();

    assert!(frame_len(&params) <= FRAME_MAX_BYTES);
    let snapshot = task_snapshot(&mut notification).unwrap();
    assert!(snapshot.truncated);
    assert!(snapshot.output.contains("/tmp/bg-1.log"));
}

/// Limiting the output field alone misses this: JSON encoding makes each of
/// these bytes six times larger.
#[test]
fn escaped_output_fits_too() {
    let mut notification = notification(&"\u{7}".repeat(30 * 1024));
    let params = encode(&mut notification).unwrap();

    assert!(frame_len(&params) <= FRAME_MAX_BYTES);
    assert!(!task_snapshot(&mut notification).unwrap().output.is_empty());
}

/// The output is not the only field a task can grow: with no output at all,
/// the frame must still fit once the other variable fields are capped.
#[test]
fn oversized_non_output_fields_are_capped_too() {
    let mut notification = notification("");
    {
        let snapshot = task_snapshot(&mut notification).unwrap();
        snapshot.command = "\u{7}".repeat(80 * 1024);
        snapshot.description = Some("d".repeat(80 * 1024));
        snapshot.cwd = format!("/{}", "c".repeat(80 * 1024));
    }
    let params = encode(&mut notification).unwrap();

    assert!(frame_len(&params) <= FRAME_MAX_BYTES);
    let snapshot = task_snapshot(&mut notification).unwrap();
    assert!(encoded_len(&snapshot.command) <= FIELD_MAX_BYTES);
}

#[test]
fn a_long_log_path_still_fits() {
    let mut notification = notification(&"Z".repeat(64 * 1024));
    task_snapshot(&mut notification).unwrap().output_file =
        PathBuf::from(format!("/tmp/{}/task.log", "p".repeat(30 * 1024)));
    let params = encode(&mut notification).unwrap();

    assert!(frame_len(&params) <= FRAME_MAX_BYTES);
    // A long path costs output, but the pointer stays whole: a truncated one
    // would send the reader nowhere.
    let snapshot = task_snapshot(&mut notification).unwrap();
    assert!(
        snapshot
            .output_file
            .display()
            .to_string()
            .ends_with("/task.log")
    );
}

#[test]
fn the_encoded_message_matches_the_one_left_on_the_notification() {
    let mut notification = notification(&"Z".repeat(2 * 1024 * 1024));
    let params = encode(&mut notification).unwrap();

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(params.get()).unwrap(),
        serde_json::to_value(&notification).unwrap()
    );
}

/// A cut output can still be longer than the original once the footer is
/// added, so the mark cannot come from comparing lengths. The squeeze uses the
/// session id because compaction does not give that room back.
#[test]
fn output_cut_to_fit_is_always_marked_incomplete() {
    const ROOM_FOR_OUTPUT: usize = 716;

    let mut notification = notification("");
    let filled = serde_json::to_string(&notification).unwrap().len();
    let pad = body_budget().saturating_sub(filled + ROOM_FOR_OUTPUT);
    let output = "\u{7}".repeat(120);
    {
        let snapshot = task_snapshot(&mut notification).unwrap();
        snapshot.owner_session_id = Some("s".repeat(pad));
        snapshot.output = output.clone();
    }
    assert!(encoded_len(&output) > ROOM_FOR_OUTPUT);

    encode(&mut notification).unwrap();

    let snapshot = task_snapshot(&mut notification).unwrap();
    assert!(!snapshot.output.is_empty(), "some output must survive");
    assert!(snapshot.output.len() > output.len());
    assert!(snapshot.truncated, "a cut output must be marked incomplete");
}

/// Nothing is returned unmeasured. When even the ids do not fit, there is no
/// message to send, and sending one anyway is what closes the connection.
#[test]
fn a_message_that_cannot_fit_at_all_is_not_returned() {
    let mut notification = notification("hello");
    task_snapshot(&mut notification).unwrap().task_id = "t".repeat(FRAME_MAX_BYTES);

    assert!(encode(&mut notification).is_none());
}

/// A recorded completion that fits no other way loses its output and gets a
/// capped path, the same last resort the live path takes, instead of being
/// dropped.
#[test]
fn a_recorded_completion_with_an_oversized_path_is_refit_not_dropped() {
    let mut record = serde_json::to_value(notification(&"Z".repeat(64 * 1024))).unwrap();
    record["update"]["task_snapshot"]["output_file"] =
        serde_json::Value::String(format!("/tmp/{}/task.log", "p".repeat(80 * 1024)));
    let raw = serde_json::value::to_raw_value(&record).unwrap();

    match refit_recorded(&raw) {
        Refit::Fitted(fitted) => assert!(fitted.get().len() <= body_budget()),
        Refit::Unchanged | Refit::Unfittable => {
            panic!("a record the live path can send must not be dropped on replay")
        }
    }
}

/// A recorded completion whose command alone is oversized is refit, not
/// dropped: the replay path caps the same fields the live path does.
#[test]
fn a_recorded_completion_with_an_oversized_command_is_refit() {
    let mut record = serde_json::to_value(notification("small\n")).unwrap();
    record["update"]["task_snapshot"]["command"] = serde_json::Value::String("c".repeat(80 * 1024));
    let raw = serde_json::value::to_raw_value(&record).unwrap();

    match refit_recorded(&raw) {
        Refit::Fitted(fitted) => assert!(fitted.get().len() <= body_budget()),
        Refit::Unchanged | Refit::Unfittable => {
            panic!("an oversized recorded command must be refit")
        }
    }
}

/// The wrapper reservation, re-derived here from the JSON-RPC line shape
/// the bridge sends. A transport change shows up as this test going stale,
/// not as proof against it.
#[test]
fn the_reservation_matches_the_line_the_transport_writes() {
    let body = "{}";
    let line = format!(r#"{{"jsonrpc":"2.0","method":"_{METHOD}","params":{body}}}"#) + "\n";

    assert_eq!(line.len() - body.len(), WRAPPER_BYTES + METHOD.len());
    // Replay sends its own method through the same budget.
    assert!("echo.agent/session/update".len() <= METHOD.len());
}

#[test]
fn encoded_len_matches_the_encoder() {
    for text in [
        "plain",
        "quote\" and backslash\\",
        "newline\ntab\treturn\r",
        "control\u{1}\u{7}\u{1f}",
        "not escaped by the encoder: \u{7f}\u{80}\u{9f}",
        "unicode: 日本語 🎉",
    ] {
        let written = serde_json::to_string(text).unwrap();
        assert_eq!(
            encoded_len(text) + 2, // the surrounding quotes
            written.len(),
            "mismatch for {text:?}"
        );
    }
}
