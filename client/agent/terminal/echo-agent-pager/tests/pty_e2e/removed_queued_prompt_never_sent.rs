// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 20. **Removed queued prompt never reaches the wire.**
/// Enter mid-turn queues; the queue pane lists rows as `#N`. Removing row
/// 1 with `x` must keep its text out of every subsequent request, while
/// the surviving prompt promotes FIFO after the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn removed_queued_prompt_never_sent() {
    let content = ContentController::start().await.expect("start content");
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn while queued prompt is removed",
        slow_turn_text("TURNONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "surviving queued prompt",
        "TURNTWO promoted prompt response.",
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
        .wait_for_text("TURNONE", Duration::from_secs(30))
        .expect("turn 1 streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(b"queued alpha\r")
        .expect("queue first prompt");
    harness
        .wait_for_text("queued alpha", Duration::from_secs(10))
        .expect("queue pane shows first row");
    harness
        .inject_keys(b"queued bravo\r")
        .expect("queue second prompt");
    harness
        .wait_for_text("queued bravo", Duration::from_secs(10))
        .expect("queue pane shows second row");

    // Focus the auto-shown pane (visible+unfocused -> focused) and remove
    // row 1; selection starts on the first row.
    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("focus queue pane");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"x").expect("remove first row");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while harness.contains_text("queued alpha") {
        assert!(
            std::time::Instant::now() < deadline,
            "removed row still on screen\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }

    // `queued alpha` vanished optimistically the instant `x` was handled; give
    // the `echo.agent/queue/remove` RPC time to reach the shell and mutate the
    // authoritative queue before we let turn 1 complete. Turn 1 stays gated
    // throughout, so the removal always lands while `alpha` is still queued
    // (never promoted) — the survivor `bravo` is the only thing left to run.
    harness.update(Duration::from_millis(500));

    // Now let turn 1 finish: the sole survivor `queued bravo` promotes FIFO
    // into turn 2.
    turn_one.release();

    // Assert promotion on the WIRE, not on scrollback text. The auto-shown
    // queue pane overlays the top of the scrollback, so the promoted turn's
    // response can render behind it — a `wait_for_text` on the response is a
    // flaky observation, not a real failure. The request bodies are the
    // authoritative record of what was actually sent.
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    while !all_user_messages(&content)
        .iter()
        .any(|u| u.contains("queued bravo"))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "surviving prompt never promoted after turn 1\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }

    let users = all_user_messages(&content);
    assert!(
        users.iter().any(|u| u.contains("queued bravo")),
        "surviving prompt never sent: {users:#?}"
    );
    assert!(
        !users.iter().any(|u| u.contains("queued alpha")),
        "removed prompt reached the wire: {users:#?}"
    );

    harness.quit().expect("clean quit");
}
