// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 6. **Agent type mismatch — "Yes" starts a new session.**
/// Selecting "Yes" on the modal creates a new session with the target
/// model active.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn agent_type_mismatch_yes_starts_new_session() {
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

    // Select "Yes" (first option, already focused by default).
    harness.inject_keys(keys::ENTER).expect("select yes");

    // A new session should be created — look for the session tip or
    // the fresh prompt (no scrollback from the previous session).
    harness
        .wait_for_text("Session", Duration::from_secs(15))
        .expect("new session created");

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited after starting new session\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
