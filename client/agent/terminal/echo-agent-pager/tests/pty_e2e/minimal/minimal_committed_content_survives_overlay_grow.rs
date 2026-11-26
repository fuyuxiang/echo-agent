// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Overlay host: growing the live viewport for an overlay must
/// scroll committed rows up into native scrollback (the `set_viewport_height`
/// grow fix) rather than clobbering them, and shrinking it back when the overlay
/// closes must leave them intact. Commit a tall response into scrollback, open
/// the slash dropdown (grows the viewport over committed rows), close it
/// (shrinks + re-anchors), and assert the committed head survived in scrollback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_committed_content_survives_overlay_grow() {
    let content = ContentController::start().await.expect("start content");
    // Sentinel on the first rendered row; 80 code-block rows overflow the screen
    // so the head reaches native scrollback (prose would reflow to fit on screen
    // and never scroll — see `tall_response`).
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
        "precondition: committed block must reach scrollback before the overlay\nscrollback:\n{}",
        harness.scrollback_text()
    );

    // Open the slash dropdown → grows the live viewport over committed rows.
    inject_keys_paced(&mut harness, b"/mod");
    let dropdown_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < dropdown_deadline
        && !harness
            .screen_contents()
            .contains("Switch the active model")
    {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness
            .screen_contents()
            .contains("Switch the active model"),
        "slash dropdown must grow the viewport and render its items even when \
         committed content fills the screen\nscreen:\n{}\nscrollback:\n{}",
        harness.screen_contents(),
        harness.scrollback_text(),
    );
    // Close it → shrinks + re-anchors to the bottom.
    harness.inject_keys(keys::ESC).expect("close dropdown");
    harness.update(Duration::from_millis(400));

    // The committed head must still be readable in scrollback: the grow/shrink
    // cycle must neither clobber nor lose it.
    assert!(
        harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL),
        "committed block must survive the overlay grow/shrink cycle\nscrollback:\n{}",
        harness.scrollback_text()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
