// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// `/settings` opens the full settings editor inline in minimal mode —
/// hosted in the grown live viewport, reusing the real `render_settings_modal`
/// so behavior matches the full TUI — and Esc closes it back to the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_settings_modal_opens_and_closes() {
    let content = ContentController::start().await.expect("start content");
    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    // Open the settings editor (paced so the slash dropdown opens rather than
    // the bytes paste-coalescing), then submit.
    inject_keys_paced(&mut harness, b"/settings");
    harness.inject_keys(b"\r").expect("submit /settings");

    // "Appearance" is the first settings category header — it renders only in
    // the settings editor, never in the status line or the slash dropdown.
    harness
        .wait_for_text("Appearance", Duration::from_secs(10))
        .expect("settings editor renders inline");

    // Esc closes it; the idle prompt status returns and the editor is gone.
    harness.inject_keys(keys::ESC).expect("close settings");
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(10))
        .expect("settings closed, back to the prompt");
    assert!(
        !harness.contains_text("Appearance"),
        "settings editor must be gone after Esc\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
