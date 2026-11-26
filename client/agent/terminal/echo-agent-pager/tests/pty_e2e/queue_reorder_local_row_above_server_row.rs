// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A slash command is a client row and the pane draws every shell row first, so moving one up must cross that boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn queue_reorder_local_row_above_server_row() {
    let content = ContentController::start().await.expect("start content");
    // Gate the turn so both rows provably queue while it is still running.
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before local row reorder",
        slow_turn_text("LOCALREORDER"),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("LOCALREORDER", Duration::from_secs(45))
        .expect("turn streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn reached completion barrier");

    // A plain prompt mid-turn goes to the shell queue.
    harness
        .inject_keys(b"reorder gamma\r")
        .expect("queue a shell row");
    harness
        .wait_for_text("reorder gamma", Duration::from_secs(20))
        .expect("shell row queued");
    // A slash command stays in the client queue, so the pane draws it last.
    harness
        .inject_keys(b"/compact keep-this-note\r")
        .expect("queue a client row");
    harness
        .wait_for_text("keep-this-note", Duration::from_secs(20))
        .expect("client row queued");

    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("focus queue pane");
    harness.update(Duration::from_millis(300));
    // Select the last row (the slash command) and move it to the top.
    harness.inject_keys(b"j").expect("select the second row");
    harness.update(Duration::from_millis(200));
    for _ in 0..3 {
        harness.inject_keys(b"K").expect("move the row up");
        harness.update(Duration::from_millis(300));
    }

    let screen = harness.screen_contents();
    let slash_row = locate_screen_text(&screen, "keep-this-note")
        .unwrap_or_else(|| panic!("slash row on screen:\n{screen}"))
        .0;
    let prompt_row = locate_screen_text(&screen, "reorder gamma")
        .unwrap_or_else(|| panic!("prompt row on screen:\n{screen}"))
        .0;
    assert!(
        slash_row < prompt_row,
        "the slash command must move above the queued prompt; slash at row {slash_row}, prompt at row {prompt_row}\nscreen:\n{screen}"
    );

    turn_one.release();
}
