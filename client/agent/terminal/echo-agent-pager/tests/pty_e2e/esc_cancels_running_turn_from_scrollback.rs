// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// **1× Esc from the SCROLLBACK pane cancels a running turn** in the default
/// (non-vim) config. The policy treats Prompt and Scrollback identically while
/// a turn runs, so a user reading the transcript can interrupt without first
/// returning to the prompt. Tab (not Esc) is used to leave the prompt; the
/// footer's "Space:prompt" hint confirms the scrollback owns keys before the
/// cancel Esc is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn esc_cancels_running_turn_from_scrollback() {
    let content = ContentController::start().await.expect("start content");
    let long_response = format!(
        "{MOCK_RESPONSE_SENTINEL} {}",
        "streaming filler words for the cancellation window. ".repeat(120)
    );
    content.set_response(long_response);
    content.set_chunk_delay(Some(Duration::from_millis(50)));

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
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("stream started");

    // Leave the prompt with a SINGLE Tab (Esc is reserved for cancel/clear/
    // rewind), then wait for the footer to prove the scrollback owns keys. Tab
    // TOGGLES focus, so re-pressing it could bounce focus back to the prompt —
    // press once and poll the render instead (mirrors `drive_to_scrollback_with_turn`).
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback must own keys before the cancel Esc");

    // 1× Esc from scrollback cancels the running turn.
    harness.inject_keys(keys::ESC).expect("press esc");
    harness.update(Duration::from_millis(200));

    harness
        .wait_for_text("Turn cancelled by user", Duration::from_secs(15))
        .expect("turn cancelled marker (from scrollback)");

    harness.update(Duration::from_millis(600));
    let screen = harness.screen_contents();
    assert_eq!(
        screen.matches("Turn cancelled by user").count(),
        1,
        "'Turn cancelled' must appear exactly once\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
