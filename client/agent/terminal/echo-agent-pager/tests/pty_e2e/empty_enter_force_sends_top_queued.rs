// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Mid-turn: queue a follow-up with Enter, then bare Enter on the empty
/// composer sends that top row now — cancel-and-send: the running turn is
/// cancelled silently and the row runs as its own next turn, arriving on the
/// wire as a standard `<user_query>` prompt with the interjection preamble.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn empty_enter_force_sends_top_queued() {
    let content = ContentController::start().await.expect("start content");
    let mut turn_one = content
        .expect_agent_turn_blocked("running turn before send-now", slow_turn_text("TURNONE"));
    let mut turn_two = content.expect_agent_turn(
        "promoted queued follow-up",
        "TURNTWO reply to the promoted follow-up.",
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
        .expect("turn 1 reached the completion barrier");

    harness
        .inject_keys(b"please also check the logs\r")
        .expect("queue follow-up via Enter");
    harness
        .wait_for_text("please also check the logs", Duration::from_secs(10))
        .expect("queued text visible");

    // Composer is empty after queue; bare Enter sends the top row now. The
    // shell cancels turn 1 (the abort beats the held completion) and promotes
    // the row to run as turn 2.
    harness.inject_keys(b"\r").expect("empty Enter send-now");
    turn_one.release();
    // The promoted row renders as a standard "❯ " prompt block via the
    // turn-start adoption (the arrow prefix distinguishes the committed block
    // from the prefix-less queue row).
    harness
        .wait_for_text(
            "\u{276F} please also check the logs",
            Duration::from_secs(15),
        )
        .expect("promoted prompt scrollback chrome");

    harness
        .wait_for_text("TURNTWO", Duration::from_secs(40))
        .expect("promoted turn reply");
    tokio::time::timeout(Duration::from_secs(10), turn_two.wait_satisfied())
        .await
        .expect("promoted turn expectation satisfied");

    // The send-now cancel is silent: no cancelled marker between the partial
    // turn-1 output and the promoted prompt.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    let users = all_user_message_blobs(&content);
    let promoted = users
        .iter()
        .find(|u| u.contains("please also check the logs"))
        .unwrap_or_else(|| panic!("queued follow-up never reached the wire: {users:#?}"));
    assert!(
        promoted.contains(INTERJECTION_WIRE_PREFIX),
        "send-now must use the interjection preamble: {promoted}"
    );
    assert!(
        promoted.contains("<user_query>"),
        "send-now must wrap the steered text in user_query: {promoted}"
    );
    assert!(
        promoted.contains("Make sure to complete any unfinished tasks from previous turns."),
        "send-now must keep the unfinished-task trailer: {promoted}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
