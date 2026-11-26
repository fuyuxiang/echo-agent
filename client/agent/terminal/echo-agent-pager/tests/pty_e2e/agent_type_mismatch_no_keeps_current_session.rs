// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 7. **Agent type mismatch — "No" returns to current session.**
/// Selecting "No" dismisses the modal and keeps the current session
/// with its original model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn agent_type_mismatch_no_keeps_current_session() {
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

    // Wait for the modal.
    harness
        .wait_for_text("requires starting a new session", Duration::from_secs(15))
        .expect("modal appeared");

    // Select "No" (second option — press j then Enter).
    harness.inject_keys(keys::J).expect("move to No");
    harness.inject_keys(keys::ENTER).expect("select no");

    // Modal should dismiss. Wait a moment for UI to settle.
    harness.update(Duration::from_millis(500));

    // The original response should still be visible (same session).
    assert!(
        harness.contains_text(MOCK_RESPONSE_SENTINEL),
        "original response should still be visible after cancelling\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("requires starting a new session"),
        "modal should be dismissed\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
