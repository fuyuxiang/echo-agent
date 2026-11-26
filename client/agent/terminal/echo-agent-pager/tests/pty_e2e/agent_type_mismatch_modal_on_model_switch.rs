// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 5. **Agent type mismatch — modal appears on `/model` switch.**
/// After sending a prompt (turn_count > 0), switching to a model with a
/// different agent type shows the question modal instead of a raw error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn agent_type_mismatch_modal_on_model_switch() {
    let content = start_dual_agent_type_content().await;
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} hello from the default harness."
    ));

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

    harness
        .inject_keys(b"/model cursor-model\r")
        .expect("type model switch");

    // The question modal should appear.
    harness
        .wait_for_text("requires starting a new session", Duration::from_secs(15))
        .expect("agent type mismatch modal should appear");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
