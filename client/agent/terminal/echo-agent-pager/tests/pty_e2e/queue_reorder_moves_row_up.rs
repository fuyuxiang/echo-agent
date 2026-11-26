// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A reorder must change the order the shell drains rows in, so this asserts on the recorded requests, not on the pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn queue_reorder_moves_row_up() {
    let content = ContentController::start().await.expect("start content");
    // Gate turn 1 so both prompts provably queue while it is still the running turn.
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before queue reorder",
        slow_turn_text("REORDERONE"),
    );
    let _turn_two = content.expect_agent_turn("first drained prompt", "REORDERTWO handled.");
    let _turn_three = content.expect_agent_turn("second drained prompt", "REORDERTHREE handled.");

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
        .wait_for_text("REORDERONE", Duration::from_secs(45))
        .expect("turn 1 streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(b"reorder alpha\r")
        .expect("queue alpha");
    harness
        .wait_for_text("reorder alpha", Duration::from_secs(20))
        .expect("alpha queued");
    harness.inject_keys(b"reorder beta\r").expect("queue beta");
    harness
        .wait_for_text("reorder beta", Duration::from_secs(20))
        .expect("beta queued");

    // Focus the queue, select the second row, and move it up.
    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("focus queue pane");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"j").expect("select the second row");
    harness.update(Duration::from_millis(200));
    harness.inject_keys(b"K").expect("move the row up");
    harness.update(Duration::from_millis(500));
    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("unfocus queue pane");

    turn_one.release();
    harness
        .wait_for_text("REORDERTHREE", Duration::from_secs(90))
        .expect("both queued prompts drained");

    // The wire proves the shell honored the move: beta must run before alpha.
    let queries: Vec<String> = content
        .request_bodies()
        .last()
        .expect("final request recorded")["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .filter(|c| c.contains("reorder "))
        .collect();
    let alpha = queries.iter().position(|q| q.contains("reorder alpha"));
    let beta = queries.iter().position(|q| q.contains("reorder beta"));
    assert!(
        matches!((alpha, beta), (Some(a), Some(b)) if b < a),
        "the moved row must drain first; got alpha at {alpha:?}, beta at {beta:?}\nqueries: {queries:#?}"
    );
}
