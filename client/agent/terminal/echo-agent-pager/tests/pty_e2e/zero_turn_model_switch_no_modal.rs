// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 8. **Zero-turn model switch — no modal, silent rebuild.**
/// Switching to a model with a different agent type before any prompts
/// are sent should succeed silently (shell auto-rebuilds at turn_count == 0).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn zero_turn_model_switch_no_modal() {
    let content = start_dual_agent_type_content().await;

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Switch models BEFORE sending any prompt.
    harness
        .inject_keys(b"/model cursor-model\r")
        .expect("type model switch");

    // The "Switched to" scrollback line is raced away when the switch lands
    // before the session exists (deferred_model_switch applies silently on
    // SessionCreated). Converge on signals common to both paths: prompt
    // consumed and the status bar showing the new model.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        harness.update(Duration::from_millis(100));
        if !harness.contains_text("/model") && harness.contains_text("cursor-model") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for zero-turn model switch\nscreen:\n{}",
            harness.screen_contents()
        );
    }

    // No modal should appear.
    assert!(
        !harness.contains_text("requires starting a new session"),
        "modal should NOT appear for zero-turn switch\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
