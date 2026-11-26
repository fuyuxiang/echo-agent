// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The no-rewind boundary: once the server has streamed ANY activity, Ctrl+C
/// is a standard cancel — the prompt stays a committed "❯ " block (exactly
/// once, NOT restored to the composer) and the "Turn cancelled by user"
/// marker renders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ctrlc_after_activity_no_rewind_prompt_once() {
    const CANCEL_PROMPT: &str = "cancel after activity";

    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let _cancelled_turn = content.expect_agent_turn(
        "turn cancelled after visible activity",
        slow_turn_text("CANCELME"),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{CANCEL_PROMPT}\r").as_bytes())
        .expect("submit prompt");
    // Server activity on screen — the rewind window is provably closed.
    harness
        .wait_for_text("CANCELME", Duration::from_secs(30))
        .expect("turn streaming");

    harness.inject_keys(keys::CTRL_C).expect("Ctrl+C cancel");
    harness
        .wait_for_text("Turn cancelled by user", Duration::from_secs(10))
        .expect("standard cancel marker");

    // No rewind: the composer stays empty and the committed block stays put,
    // exactly once.
    assert!(
        !composer_holds(&harness, CANCEL_PROMPT),
        "post-activity cancel must not restore the prompt to the composer\nscreen:\n{}",
        harness.screen_contents()
    );
    assert_eq!(
        block_lines_containing(&harness, CANCEL_PROMPT),
        1,
        "prompt must stay committed exactly once\nscreen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
