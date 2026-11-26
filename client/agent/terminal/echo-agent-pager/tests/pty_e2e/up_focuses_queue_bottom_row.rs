// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Up on an empty composer moves focus into the queue on its BOTTOM row rather than opening prompt history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn up_focuses_queue_bottom_row() {
    const PROMPT_A: &str = "alpha primary task";
    const QUEUED_FIRST: &str = "bravo queued first";
    const QUEUED_LAST: &str = "charlie queued last";
    const EDIT_SUFFIX: &str = " EDITEDHERE";
    const UP: &[u8] = b"\x1b[A";

    let content = ContentController::start().await.expect("start content");
    // Hold turn A open so both follow-ups provably queue rather than send.
    let mut turn_a =
        content.expect_agent_turn_blocked("running turn A", slow_turn_text("ALPHARESP"));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT_A}\r").as_bytes())
        .expect("submit A");
    harness
        .wait_for_text("ALPHARESP", Duration::from_secs(45))
        .expect("A streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_a.wait_blocked())
        .await
        .expect("turn A reached completion barrier");

    for text in [QUEUED_FIRST, QUEUED_LAST] {
        harness
            .inject_keys(format!("{text}\r").as_bytes())
            .expect("queue mid-turn");
        harness
            .wait_for_text(text, Duration::from_secs(20))
            .expect("queued row visible");
    }

    // `e` edits whatever row the highlight sits on, so the recalled text names the row Up selected.
    harness.inject_keys(UP).expect("Up into the queue");
    harness.inject_keys(b"e").expect("edit the highlighted row");
    harness
        .inject_keys(EDIT_SUFFIX.as_bytes())
        .expect("type into the recalled row");

    let edited_last = format!("{QUEUED_LAST}{EDIT_SUFFIX}");
    harness
        .wait_for_text(&edited_last, Duration::from_secs(20))
        .expect("bottom row loaded into the composer");
    assert!(
        !harness.contains_text(&format!("{QUEUED_FIRST}{EDIT_SUFFIX}")),
        "Up selected the top row, not the bottom\nscreen:\n{}",
        harness.screen_contents()
    );

    turn_a.release();

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
