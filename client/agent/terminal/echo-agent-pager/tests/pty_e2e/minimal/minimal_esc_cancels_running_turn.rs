// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Esc cancels a running turn in minimal mode (the prompt is always focused, so
/// the turn-running Esc branch wins; minimal enables the Esc-cancel gate
/// regardless of vim mode). The cancellation marker is finalized and committed
/// to native scrollback like any other block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_esc_cancels_running_turn() {
    let content = ContentController::start().await.expect("start content");
    // Paced, long stream so the turn is provably still running when Esc lands.
    let long = format!(
        "{MOCK_RESPONSE_SENTINEL} {}",
        "streaming filler words for the cancellation window. ".repeat(120)
    );
    content.set_response(long);
    content.set_chunk_delay(Some(Duration::from_millis(50)));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn streaming in the live tail");

    harness.inject_keys(keys::ESC).expect("press esc to cancel");

    // Full-text: minimal commits the cancel marker to native scrollback, so it
    // may sit above the pinned viewport — check scrollback + screen.
    harness
        .wait_for_full_text("Turn cancelled by user", Duration::from_secs(15))
        .expect("cancellation marker committed to scrollback");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
