// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Resize hazard: minimal mode uses only the built-in viewport
/// machinery and never re-emits committed history (`resize_purge_rerender` /
/// `emit_to_scrollback` are forbidden). The terminal reflows committed
/// scrollback natively on resize. This commits a tall response into native
/// scrollback, resizes the terminal smaller (rows AND cols), and asserts the
/// committed content survives (not wiped, not double-printed), the pager does
/// not panic or exit, and the prompt is still functional afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_resize_preserves_committed_scrollback() {
    let content = ContentController::start().await.expect("start content");
    // Sentinel on the first rendered row; 80 code-block rows >> screen, so the
    // head scrolls into native scrollback once the block commits. (Prose would
    // markdown-reflow into one short on-screen paragraph — see `tall_response`.)
    content.set_response(tall_response(MOCK_RESPONSE_SENTINEL, 80));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Precondition: the committed head must be in native scrollback first.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && !harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL) {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL),
        "precondition: committed block must reach scrollback before the resize\nscrollback:\n{}",
        harness.scrollback_text()
    );

    // Resize smaller in both dimensions. The terminal reflows committed history
    // natively; minimal must neither reprint (double-print) nor wipe it.
    harness.resize(30, 80).expect("resize smaller");
    harness.update(Duration::from_millis(800));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during resize\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked after resize\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_full_text(MOCK_RESPONSE_SENTINEL),
        "committed content must survive the resize reflow\nfull:\n{}",
        harness.full_text()
    );

    // The prompt is still functional: a second turn streams after the resize.
    content.set_response(format!("{} after resize.", turn_sentinel(2)));
    harness
        .inject_keys(b"again\r")
        .expect("submit a second turn after resize");
    harness
        .wait_for_full_text(&turn_sentinel(2), Duration::from_secs(30))
        .expect("second turn streams after the resize");

    quit_minimal(&mut harness);
}
