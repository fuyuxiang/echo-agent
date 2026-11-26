// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 9. **Same agent_type switch — no modal, normal switch.**
/// Switching between two models that share the same agent type (or no
/// agent type) mid-session should succeed normally without any modal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn same_agent_type_switch_no_modal() {
    // Both models have no agent_type → both use echo-agent-build harness.
    let content = ContentController::start_with_models(vec![
        MockModel::new("model-a"),
        MockModel::new("model-b"),
    ])
    .await
    .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} hello from model-a."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Send a prompt to establish turn_count > 0.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response rendered");

    // Switch to model-b (same agent type).
    harness
        .inject_keys(b"/model model-b\r")
        .expect("type model switch");

    // Should switch normally — look for the model name in status bar
    // (bottom-right). The toast "✓ Default model: model-b" may be
    // transient, so check for "model-b" anywhere on screen.
    harness
        .wait_for_text("model-b", Duration::from_secs(15))
        .expect("model-b visible on screen after switch");

    assert!(
        !harness.contains_text("requires starting a new session"),
        "modal should NOT appear for same-agent-type switch\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
