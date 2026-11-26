// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 4. **Scroll.**
/// After receiving a long agent response, scrolling keeps the pager
/// running and doesn't render a `panicked` string anywhere on screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scroll_does_not_crash() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(long_response(MOCK_RESPONSE_SENTINEL, 200));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response on screen");

    // Let the stream finish so we have plenty to scroll through.
    harness.update(Duration::from_millis(500));

    for _ in 0..40 {
        harness.inject_keys(keys::J).expect("scroll down");
        harness.update(Duration::from_millis(20));
    }
    harness.update(Duration::from_millis(250));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during scroll\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked' during scroll\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
