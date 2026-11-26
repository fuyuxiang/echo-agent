//! PTY: a user prompt queued behind a running auto-wake turn must survive a
//! Ctrl+C cancel — it runs next and is durable across `--continue`.
//!
//! The failure chain this guards: a background task completes while the agent
//! is idle, so the shell injects a synthetic `task-completed-<id>` prompt
//! (auto-wake) whose reminder tells the model to poll the task-output tool.
//! That tool result triggers the consumed-completion sweep of
//! `pending_inputs`, which must NOT delete the running auto-wake turn's own
//! front slot. If it does, a user prompt queued behind it (the pager doesn't
//! adopt synthetic turns, so a typed message dispatches immediately) shifts to
//! the front, and the next Ctrl+C resolves THE USER'S prompt as Cancelled: it
//! never reaches the model, and — since user messages are only persisted when
//! their turn starts — it is silently gone after a `--continue` resume.
//!
//! Set `ECHO_AGENT_PTY_CAST_DIR` to also dump asciinema casts of both pager runs
//! (written before the final asserts so a failing run still produces them).
//!
//! The scenario body is shared with the Esc and [stop]-click mirrors (see
//! `auto_wake_cancel_via_esc_…` / `auto_wake_cancel_via_stop_click_…`) via
//! [`run_wake_cancel_scenario`].
#[allow(unused_imports)]
use super::common::*;

/// Marker for the user's mid-auto-wake message. Unique enough to grep for in
/// request bodies and replayed history without false positives.
#[cfg(unix)]
const CLARIFY_MARKER: &str = "CLARIFY_MARKER_XYZ";

#[cfg(unix)]
const POST_CANCEL_MARKER: &str = "POST_CANCEL_MARKER_XYZ";

#[cfg(unix)]
const UNWANTED_AUTO_WAKE_SENTINEL: &str = "UNWANTED_AUTO_WAKE_SENTINEL_XYZ";

/// Background sleep that triggers the auto-wake on completion. Long enough
/// that turn 1 settles and the auto-wake scripts are enqueued before it fires,
/// even on a loaded CI host.
#[cfg(unix)]
const BG_SLEEP_SECS: &str = "6";

/// Foreground sleep that holds the auto-wake turn deterministically running
/// while the user message and Ctrl+C are injected. Never runs to completion —
/// the cancel kills it on both the broken and fixed paths — so a generous
/// bound costs nothing and removes the settle-before-cancel race.
#[cfg(unix)]
const HOLD_SLEEP_SECS: &str = "15";

/// Which cancel gesture the scenario drives. All three ride the same
/// `session/cancel` wire; only the input path differs. Esc and StopClick
/// also gate on the wake stop affordance ([stop] while the pane is idle),
/// which only exists with the wake-turn cancel support.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum WakeCancelGesture {
    CtrlC,
    Esc,
    StopClick,
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn auto_wake_cancel_preserves_queued_user_prompt() {
    run_wake_cancel_scenario(WakeCancelGesture::CtrlC, "auto_wake_repro").await;
}

/// Shared body for the three gesture tests (Ctrl+C here, Esc and a [stop]
/// click in their mirror files). `cast_prefix` keeps the optional asciinema
/// dumps distinct per gesture.
#[cfg(unix)]
pub(crate) async fn run_wake_cancel_scenario(gesture: WakeCancelGesture, cast_prefix: &str) {
    let content = ContentController::start().await.expect("start content");

    // Turn 1: the model backgrounds a sleep via run_terminal_command, then the
    // follow-up turn settles to plain text so the agent goes idle while the
    // background task runs (the precondition for an auto-wake).
    let bg_args = json!({
        "command": format!("/bin/sleep {BG_SLEEP_SECS}"),
        "description": "auto-wake trigger",
        "is_background": true
    })
    .to_string();
    let _background_turn =
        expect_tool_turn(&content, "call_bg_wake", "run_terminal_command", bg_args);
    content.set_response("TURN1_SETTLED");

    let binary = pager_binary().expect("resolve pager binary");
    // --yolo skips the bash permission prompt; --trust skips the folder-trust gate.
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("TURN1_SETTLED", Duration::from_secs(45))
        .unwrap_or_else(|_| {
            panic!(
                "background tool call never settled; screen:\n{}",
                harness.screen_contents()
            )
        });

    // The runtime task id (a UUID minted by the terminal actor, NOT the
    // scripted tool_call_id) rides in the tool result of turn 1's follow-up
    // request, inside a <task-id>…</task-id> envelope.
    let task_id = poll_for(Duration::from_secs(10), || {
        content
            .request_bodies()
            .iter()
            .find_map(|b| extract_task_id(&b.to_string()))
    })
    .unwrap_or_else(|| {
        panic!(
            "no <task-id> in any request body\n--- non-system messages ---\n{}",
            dump_non_system_messages(&content.request_bodies())
        )
    });

    // Enqueue the auto-wake turn's scripts BEFORE the background sleep
    // completes: first a task-output poll (whose completed result triggers the
    // consumed-completion sweep), then a foreground sleep that pins the turn
    // running while the user message and Ctrl+C land.
    let poll_args = json!({ "task_ids": [task_id.clone()] }).to_string();
    let _poll_turn = expect_tool_turn(
        &content,
        "call_wake_poll",
        "get_command_or_subagent_output",
        poll_args,
    );
    let hold_args = json!({
        "command": format!("/bin/sleep {HOLD_SLEEP_SECS}"),
        "description": "hold turn"
    })
    .to_string();
    let _hold_turn = expect_tool_turn(
        &content,
        "call_wake_hold",
        "run_terminal_command",
        hold_args,
    );
    // Fallback for every unscripted request after the queues drain (and the
    // response the surviving user prompt streams on the fixed path).
    content.set_response("AUTO_WAKE_SETTLED");

    // Auto-wake mid-flight gate: the request AFTER the task-output tool call
    // executed carries its result ("=== Task <id> ==="). At that point the
    // sweep has run and the foreground hold is about to start.
    let wake_polled = poll_for(Duration::from_secs(30), || {
        let marker = format!("=== Task {task_id} ===");
        content
            .request_bodies()
            .iter()
            .any(|b| b.to_string().contains(&marker))
            .then_some(())
    })
    .is_some();
    assert!(
        wake_polled,
        "auto-wake turn never polled the task output\n--- non-system messages ---\n{}\n--- screen ---\n{}",
        dump_non_system_messages(&content.request_bodies()),
        harness.screen_contents()
    );
    harness.update(Duration::from_millis(500));

    // Esc / StopClick gate on the wake stop affordance first: the pane is
    // still idle here (nothing typed), so a rendered [stop] is the wake
    // turn's, and it does not exist without the wake-turn cancel support.
    if !matches!(gesture, WakeCancelGesture::CtrlC) {
        harness
            .wait_for_text("[stop]", Duration::from_secs(10))
            .unwrap_or_else(|_| {
                panic!(
                    "no [stop] affordance during the idle wake turn; screen:\n{}",
                    harness.screen_contents()
                )
            });
    }

    // The pager does not adopt synthetic turns, so it believes it is idle and
    // dispatches the typed message immediately — it queues server-side behind
    // the running auto-wake turn. Text and Enter go separately so a bulk
    // inject can't be paste-coalesced past the submit.
    harness
        .inject_keys(format!("{CLARIFY_MARKER} please stop").as_bytes())
        .expect("type clarifying message");
    harness.update(Duration::from_millis(500));
    harness
        .inject_keys(b"\r")
        .expect("submit clarifying message");
    harness.update(Duration::from_millis(500));

    // One cancel gesture: must cancel the auto-wake turn (killing the held
    // sleep), not the queued user prompt.
    match gesture {
        WakeCancelGesture::CtrlC => {
            harness.inject_keys(keys::CTRL_C).expect("press ctrl+c");
        }
        WakeCancelGesture::Esc => {
            harness.inject_keys(keys::ESC).expect("press esc");
        }
        WakeCancelGesture::StopClick => {
            harness
                .wait_for_text("[stop]", Duration::from_secs(10))
                .expect("[stop] visible before the click");
            let (row, col) = locate_screen_text(&harness.screen_contents(), "[stop]")
                .expect("locate [stop] on screen");
            // SGR press + release inside the button's hit area.
            let click = format!(
                "{}{}",
                sgr_mouse(0, row, col + 1, 'M'),
                sgr_mouse(0, row, col + 1, 'm')
            );
            harness.inject_keys(click.as_bytes()).expect("click [stop]");
        }
    }
    harness.update(Duration::from_secs(2));

    // The surviving prompt is promoted after the cancel and reaches the model.
    let marker_on_wire = poll_for(Duration::from_secs(20), || {
        content
            .request_bodies()
            .iter()
            .any(|b| b.to_string().contains(CLARIFY_MARKER))
            .then_some(())
    })
    .is_some();
    if marker_on_wire {
        // Let the promoted turn finish streaming so the resumed replay below
        // is deterministic on the fixed path.
        let _ = harness.wait_for_full_text("AUTO_WAKE_SETTLED", Duration::from_secs(15));
    }

    write_cast_if_requested(&harness, &format!("{cast_prefix}_main.cast"));

    // Graceful quit (Ctrl+Q double-press: focus is in the prompt, 'q' would type).
    harness.update(Duration::from_millis(500));
    harness.inject_keys(b"\x11").expect("ctrl-q once");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"\x11").expect("ctrl-q confirm");
    harness.quit().expect("reap pager");

    // Resume the same session: the user's message must have been persisted
    // (user messages are only written once their turn starts) and replay.
    let mut resumed = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--continue", "--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn resumed pager");
    // The replay follows the transcript tail, so on the fixed path turn 1 may
    // have scrolled above the viewport — the marker near the tail is an
    // equally valid replay-finished signal.
    let replay_ok = resumed
        .wait_for_full_text("TURN1_SETTLED", Duration::from_secs(30))
        .is_ok();
    resumed.update(Duration::from_secs(1));
    let resumed_full_text = resumed.full_text();
    let marker_in_replay = resumed.contains_full_text(CLARIFY_MARKER);

    write_cast_if_requested(&resumed, &format!("{cast_prefix}_continue.cast"));
    resumed.quit().expect("quit resumed pager");

    assert!(
        marker_on_wire,
        "{gesture:?} during the auto-wake turn destroyed the queued user prompt: \
         {CLARIFY_MARKER} never reached the model\nrequests: {}\n--- non-system messages ---\n{}",
        content.request_count(),
        dump_non_system_messages(&content.request_bodies())
    );
    assert!(
        replay_ok || marker_in_replay,
        "--continue never replayed the session history\nfull contents:\n{resumed_full_text}"
    );
    assert!(
        marker_in_replay,
        "queued user prompt missing from --continue replay (lost from history)\n\
         full contents:\n{resumed_full_text}"
    );
}

#[cfg(unix)]
fn unified_log_diagnostics(content: &ContentController) -> String {
    let path = content.home().join(".echo-agent/logs/unified.jsonl");
    let log = std::fs::read_to_string(path).unwrap_or_default();
    let mut tail: Vec<&str> = log.lines().rev().take(80).collect();
    tail.reverse();
    let relevant = log
        .lines()
        .filter(|line| line.contains("task_wake") || line.contains("shell.cancel"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n--- all task_wake / shell.cancel lines ---\n{relevant}",
        tail.join("\n")
    )
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn cancel_before_task_completion_defers_auto_wake_until_user_prompt() {
    let content = ContentController::start().await.expect("start content");

    let bg_done_flag = content.home().join("post_cancel_bg_done");
    let bg_command = format!(
        "while [ ! -e {} ]; do /bin/sleep 0.2; done",
        bg_done_flag.display()
    );
    let bg_args = json!({
        "command": bg_command,
        "description": "post-cancel completion",
        "is_background": true
    })
    .to_string();
    content.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_tool_call_events(
            "call_bg_after_cancel",
            "run_terminal_command",
            &bg_args,
        )),
    );
    content.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completions_tool_call_events_with_id(
            "call_bg_after_cancel",
            "run_terminal_command",
            &bg_args,
        )),
    );

    let hold_started_flag = content.home().join("post_cancel_hold_started");
    let hold_command = format!(
        ": > {}; while true; do /bin/sleep 0.2; done",
        hold_started_flag.display()
    );
    let hold_args = json!({
        "command": hold_command,
        "description": "ordinary turn hold"
    })
    .to_string();
    content.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_tool_call_events(
            "call_hold_after_bg",
            "run_terminal_command",
            &hold_args,
        )),
    );
    content.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completions_tool_call_events_with_id(
            "call_hold_after_bg",
            "run_terminal_command",
            &hold_args,
        )),
    );
    content.set_response(UNWANTED_AUTO_WAKE_SENTINEL);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    let task_id = poll_for(Duration::from_secs(30), || {
        content
            .request_bodies()
            .iter()
            .find_map(|body| extract_task_id(&body.to_string()))
    })
    .unwrap_or_else(|| {
        panic!(
            "background task never started\n--- non-system messages ---\n{}",
            dump_non_system_messages(&content.request_bodies())
        )
    });
    let follow_up_started = poll_for(Duration::from_secs(15), || {
        hold_started_flag.exists().then_some(())
    })
    .is_some();
    assert!(
        follow_up_started,
        "foreground hold never started\n--- non-system messages ---\n{}",
        dump_non_system_messages(&content.request_bodies())
    );

    harness.inject_keys(keys::CTRL_C).expect("press ctrl+c");
    harness
        .wait_for_text("Turn cancelled by user", Duration::from_secs(15))
        .expect("ordinary turn cancelled");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("cancelled turn idle");
    assert!(
        !harness.contains_full_text("Task completed in"),
        "background task completed before release"
    );
    std::fs::write(&bg_done_flag, b"done").expect("complete background task");
    harness
        .wait_for_full_text("Task completed in", Duration::from_secs(15))
        .expect("background completion chip");
    // hold must be strictly less than timeout: wait_until_stable stamps
    // true_since after the deadline is fixed, so timeout == hold flakes under
    // load even when the condition is always true (remote --runs_per_test).
    harness
        .wait_until_stable(
            "no auto-wake response after background completion",
            Duration::from_secs(5),
            Duration::from_secs(2),
            |h| !h.contains_full_text(UNWANTED_AUTO_WAKE_SENTINEL),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{error}\n--- unified diagnostics ---\n{}",
                unified_log_diagnostics(&content)
            )
        });

    harness
        .inject_keys(POST_CANCEL_MARKER.as_bytes())
        .expect("type post-cancel prompt");
    harness.update(Duration::from_millis(300));
    harness
        .inject_keys(b"\r")
        .expect("submit post-cancel prompt");

    let reminder_on_wire = poll_for(Duration::from_secs(30), || {
        content.request_bodies().iter().find_map(|body| {
            let serialized = body.to_string();
            (serialized.contains(POST_CANCEL_MARKER)
                && serialized.contains("Background task")
                && serialized.contains("completed")
                && serialized.contains(&task_id))
            .then_some(())
        })
    })
    .is_some();
    harness
        .wait_for_full_text(UNWANTED_AUTO_WAKE_SENTINEL, Duration::from_secs(15))
        .expect("genuine user turn response");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("genuine user turn idle");
    harness
        .wait_until_stable(
            "no second completion request after the user turn",
            Duration::from_secs(5),
            Duration::from_secs(2),
            |_| {
                content
                    .request_bodies()
                    .iter()
                    .filter(|body| body.to_string().contains(POST_CANCEL_MARKER))
                    .count()
                    == 1
            },
        )
        .expect("deferred completion consumed atomically");
    write_cast_if_requested(&harness, "auto_wake_cancel_before_completion.cast");
    harness.quit().expect("quit pager");

    assert!(
        reminder_on_wire,
        "the next genuine user request must include the deferred task-completion reminder\n\
         --- non-system messages ---\n{}\n--- unified diagnostics ---\n{}",
        dump_non_system_messages(&content.request_bodies()),
        unified_log_diagnostics(&content)
    );
}
