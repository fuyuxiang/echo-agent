// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal mode's core contract: a finalized assistant block is printed once
/// into the terminal's NATIVE scrollback (via `insert_before`), not redrawn in
/// the pinned live region. We force the commit above the viewport by streaming a
/// response taller than the screen, then assert the head line — which scrolls
/// off the top into history — is readable via the harness scrollback helpers.
/// (Short responses stay on the visible static band above the live region — the
/// content-anchored live region keeps them on screen; only a response genuinely
/// taller than the screen proves content reaches *scrollback* specifically.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_commits_response_to_scrollback() {
    let content = ContentController::start().await.expect("start content");
    // Sentinel on the FIRST rendered row; 80 code-block rows >> the 50-row
    // screen, so the head scrolls into native scrollback once the block commits.
    // (Prose lines would markdown-reflow into one short paragraph that fits on
    // screen — see `tall_response`.)
    content.set_response(tall_response(MOCK_RESPONSE_SENTINEL, 80));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // The assistant block is the running turn — it only commits to native
    // scrollback once the turn ends. Poll until the head sentinel lands in
    // scrollback (above the pinned viewport), which proves it committed rather
    // than merely streaming in the live tail.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && !harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL) {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL),
        "committed assistant block must reach native scrollback\nscrollback:\n{}\nscreen:\n{}",
        harness.scrollback_text(),
        harness.screen_contents(),
    );
    assert!(
        content.has_chat_completion(),
        "mock inference server never received a chat completion\nrequests: {:?}",
        content.requests()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
