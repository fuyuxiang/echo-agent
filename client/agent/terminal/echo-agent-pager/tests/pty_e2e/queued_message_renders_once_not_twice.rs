//! PTY, flag-file driven like `endline_park_is_markerless`: the "queued
//! message appears 2x" regression. A message queued mid-turn holds through
//! the turn's sendable wait, then drains as its own turn — asserting it
//! renders exactly once as a queue row and exactly once as a "❯ " block.
#[allow(unused_imports)]
use super::common::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn queued_message_renders_once_not_twice() {
    const QUEUED_TEXT: &str = "queued exactly once probe";

    let content = ContentController::start().await.expect("start content");
    let park_flag = content.home().join("qonce_park_flag");
    let id_ready_flag = content.home().join("qonce_id_ready_flag");

    let gated_loop = |flag: &std::path::Path| {
        format!("while [ ! -e {} ]; do /bin/sleep 0.2; done", flag.display())
    };

    // Tool call 1: the flag-gated background command the wait blocks on.
    let bg_args = json!({
        "command": gated_loop(&park_flag),
        "description": "flag-gated command",
        "is_background": true
    })
    .to_string();
    let _background_turn =
        expect_tool_turn(&content, "call_qonce_bg", "run_terminal_command", bg_args);

    // Tool call 2: the flag-gated foreground hold — the mid-turn window
    // where the follow-up is queued.
    let id_hold_args = json!({
        "command": gated_loop(&id_ready_flag),
        "description": "hold for id extraction"
    })
    .to_string();
    let _id_hold_turn = expect_tool_turn(
        &content,
        "call_qonce_id_hold",
        "run_terminal_command",
        id_hold_args,
    );

    // Fallback for both post-wait turns (the parked turn's wrap-up and the
    // drained follow-up's own turn).
    content.set_response("QONCE_WRAPUP done.");

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

    // Queue the follow-up while the id-hold tool is still running (a plain
    // mid-turn queue — NOT during the wait, where Enter would deliver it).
    harness
        .inject_keys(format!("{QUEUED_TEXT}\r").as_bytes())
        .expect("queue follow-up mid-turn");
    harness
        .wait_for_text(QUEUED_TEXT, Duration::from_secs(10))
        .expect("queued row visible");

    // Tool call 3: block on the REAL task — the sendable wait the row holds
    // through.
    let wait_args = json!({
        "task_ids": [task_id],
        "timeout_ms": 600_000
    })
    .to_string();
    let _wait_turn = expect_tool_turn(
        &content,
        "call_qonce_wait",
        "get_command_or_subagent_output",
        wait_args,
    );
    std::fs::write(&id_ready_flag, b"ready").expect("release id-extraction hold");

    // The wait parks the turn with the row HELD: the status row explains the
    // hold ("1 queued, Enter to send now"; the top row is a sendable server
    // row), the park writes no marker, and the row renders exactly once.
    harness
        .wait_for_text("1 queued, Enter to send now", Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!(
                "held-queue status hint never appeared; screen:\n{}\n--- non-system messages ---\n{}",
                harness.screen_contents(),
                dump_non_system_messages(&content.request_bodies())
            )
        });
    assert!(
        !harness.contains_text("Worked for"),
        "a park writes no marker\nscreen:\n{}",
        harness.screen_contents()
    );
    assert_eq!(
        harness
            .screen_contents()
            .lines()
            .filter(|l| l.contains(QUEUED_TEXT))
            .count(),
        1,
        "held queued message must render exactly once\nscreen:\n{}",
        harness.screen_contents()
    );
    // Held means held: nothing with the queued text has hit the model yet.
    let users = all_user_message_blobs(&content);
    assert!(
        !users.iter().any(|u| u.contains(QUEUED_TEXT)),
        "held queued message must not reach the wire during the wait: {users:#?}"
    );

    // Let the wait return: the parked turn wraps up, then the held row
    // drains as its own turn.
    std::fs::write(&park_flag, b"done").expect("release flag");
    harness
        .wait_for_text(&format!("\u{276F} {QUEUED_TEXT}"), Duration::from_secs(60))
        .expect("held row drained as its own turn");

    // Exactly once end-to-end: one "❯ " block, no leftover queue row.
    let settled = wait_until(Duration::from_secs(10), || {
        harness.update(Duration::from_millis(100));
        harness
            .screen_contents()
            .lines()
            .filter(|l| l.contains(QUEUED_TEXT))
            .count()
            == 1
    });
    assert!(
        settled,
        "drained message must render exactly once\nscreen:\n{}",
        harness.screen_contents()
    );
    let users = all_user_message_blobs(&content);
    assert_eq!(
        users.iter().filter(|u| u.contains(QUEUED_TEXT)).count(),
        1,
        "drained message must reach the wire exactly once: {users:#?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
