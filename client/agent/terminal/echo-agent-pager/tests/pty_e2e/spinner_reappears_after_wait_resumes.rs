//! PTY, flag-file driven like `endline_park_is_markerless`: the pager parks
//! on a blocking wait; releasing the flag completes the task and a slow
//! continuation streams in the SAME turn. Asserts the running chrome returns
//! (regression: stale tracker waits kept the idle look after resume).
#[allow(unused_imports)]
use super::common::*;

/// Running-turn keybar hint; absent while the parked look is active
/// (see `wait_for_turn_idle` in common.rs for the same sentinel).
#[cfg(unix)]
const CANCEL_HINT: &str = "Esc:cancel";

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn spinner_reappears_after_wait_resumes() {
    let content = ContentController::start().await.expect("start content");
    let park_flag = content.home().join("spinner_park_flag");
    let id_ready_flag = content.home().join("spinner_id_ready_flag");

    let gated_loop = |flag: &std::path::Path| {
        format!("while [ ! -e {} ]; do /bin/sleep 0.2; done", flag.display())
    };

    // Tool call 1: the flag-gated background command the turn will wait on.
    let bg_args = json!({
        "command": gated_loop(&park_flag),
        "description": "flag-gated command",
        "is_background": true
    })
    .to_string();
    let _background_turn =
        expect_tool_turn(&content, "call_spinner_bg", "run_terminal_command", bg_args);

    // Tool call 2: the flag-gated foreground hold for id extraction.
    let id_hold_args = json!({
        "command": gated_loop(&id_ready_flag),
        "description": "hold for id extraction"
    })
    .to_string();
    let _id_hold_turn = expect_tool_turn(
        &content,
        "call_spinner_id_hold",
        "run_terminal_command",
        id_hold_args,
    );

    // Fallback for the post-wait continuation: a slow stream (~5s at the
    // chunk delay set just before the flag release) so the test can observe
    // the running chrome WHILE the same turn is streaming again.
    content.set_response(slow_turn_text("RESUMED_STREAM"));

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
            .find_map(|b| extract_task_id(&b.to_string()))
    })
    .unwrap_or_else(|| {
        panic!(
            "no <task-id> in any request body\n--- non-system messages ---\n{}\n--- screen ---\n{}",
            dump_non_system_messages(&content.request_bodies()),
            harness.screen_contents()
        )
    });

    // Tool call 3: block on the REAL task until it completes (600s survives
    // the wait cap; releasing the flag below completes it long before).
    let wait_args = json!({
        "task_ids": [task_id],
        "timeout_ms": 600_000
    })
    .to_string();
    let _wait_turn = expect_tool_turn(
        &content,
        "call_spinner_wait",
        "get_command_or_subagent_output",
        wait_args,
    );

    std::fs::write(&id_ready_flag, b"ready").expect("release id-extraction hold");

    // Parked look: the parked cue takes the status row (parks write no
    // transcript row) and the running chrome (cancel keybar) drops — the
    // session reads as stopped.
    harness
        .wait_for_text("1 command still running", Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!(
                "parked watching cue never appeared; screen:\n{}\n--- non-system messages ---\n{}",
                harness.screen_contents(),
                dump_non_system_messages(&content.request_bodies())
            )
        });
    harness
        .wait_for_text("send a message to interrupt", Duration::from_secs(30))
        .unwrap_or_else(|_| {
            panic!(
                "parked interrupt cue never appeared; screen:\n{}",
                harness.screen_contents()
            )
        });
    let chrome_hidden = wait_until(Duration::from_secs(10), || {
        harness.update(Duration::from_millis(100));
        !harness.contains_text(CANCEL_HINT)
    });
    assert!(
        chrome_hidden,
        "parked look must hide the running chrome ({CANCEL_HINT}); screen:\n{}",
        harness.screen_contents()
    );

    // Complete the wait: pace the continuation, then release the flag. The
    // gated command exits, the blocking wait returns its output, and the
    // model resumes streaming in the SAME turn.
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    std::fs::write(&park_flag, b"done").expect("release flag");

    harness
        .wait_for_text("RESUMED_STREAM", Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!(
                "post-wait continuation never streamed; screen:\n{}\n--- non-system messages ---\n{}",
                harness.screen_contents(),
                dump_non_system_messages(&content.request_bodies())
            )
        });

    // The running chrome is back WHILE the continuation is still streaming
    // (~5s window): status row activity + the cancel keybar hint.
    let chrome_back = wait_until(Duration::from_secs(4), || {
        harness.update(Duration::from_millis(50));
        harness.contains_text(CANCEL_HINT)
    });
    assert!(
        chrome_back,
        "running chrome must return while the resumed turn streams; screen:\n{}",
        harness.screen_contents()
    );

    // No new "❯ " block appeared: the resume continues the SAME turn.
    assert!(
        !harness.contains_text("\u{276F} RESUMED_STREAM"),
        "the resumed stream must not render as a new user turn\nscreen:\n{}",
        harness.screen_contents()
    );

    // Let the turn end cleanly (drop pacing so the tail flushes fast).
    content.set_chunk_delay(None);
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    write_cast_if_requested(&harness, "spinner_reappears_after_wait_resumes.cast");
    harness.quit().expect("clean quit");
}
