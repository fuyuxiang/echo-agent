// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// `/help` opens the command palette inline in minimal mode, hosted in
/// the grown live viewport by the generalized app-modal host (the same host
/// that renders settings) — proving minimal now renders the full `ActiveModal`
/// family, not just settings. Esc dismisses it back to the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_help_opens_command_palette() {
    let content = ContentController::start().await.expect("start content");
    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    inject_keys_paced(&mut harness, b"/help");
    harness.inject_keys(b"\r").expect("submit /help");

    // "New Session" is a stable command-palette entry that renders only inside
    // the palette modal (not the status line or the slash dropdown).
    harness
        .wait_for_text("New Session", Duration::from_secs(10))
        .expect("command palette opens inline");

    // Esc closes it. The palette opens in input mode, so the first Esc may exit
    // input mode and a second closes — press up to twice.
    for _ in 0..2 {
        harness.inject_keys(keys::ESC).expect("press esc");
        harness.update(Duration::from_millis(300));
        if !harness.contains_text("New Session") {
            break;
        }
    }
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(10))
        .expect("palette closed, back to the prompt");
    assert!(
        !harness.contains_text("New Session"),
        "command palette must be gone after Esc\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
