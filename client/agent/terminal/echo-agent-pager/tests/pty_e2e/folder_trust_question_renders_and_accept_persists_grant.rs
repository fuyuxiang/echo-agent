// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 13. **Folder-trust question renders + accept persists the grant.**
/// Feature on + an untrusted git repo with `.mcp.json` + empty store => the
/// trust question renders BEFORE any session. Pressing `y` writes the grant to
/// `trusted_folders.toml` and lets the session proceed (prompt streams back).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn folder_trust_question_renders_and_accept_persists_grant() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} trusted and running."));
    let repo = git_repo_with_mcp_json();
    let env_refs = trust_env(true);
    let cwd = repo.path().to_str().expect("utf8 repo path");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--cwd", cwd],
        &env_refs,
    )
    .expect("spawn pager");

    // The question must render before any session exists.
    harness
        .wait_for_text(TRUST_QUESTION_SENTINEL, WELCOME_TIMEOUT)
        .expect("trust question renders");
    assert!(
        !folder_is_trusted(&content, repo.path()),
        "store must be empty before the user answers",
    );

    // BUG A regression: the global `Ctrl+N` shortcut bypasses the welcome
    // interceptor (it confirms a pending action consumed above the interceptor),
    // but the dispatch chokepoint must still refuse to create a session while
    // trust is Pending. Two presses (NewSession requires confirmation) must
    // leave us on the trust question, NOT in a session.
    harness.inject_keys(b"\x0e").expect("inject Ctrl+N"); // arm
    harness.inject_keys(b"\x0e").expect("inject Ctrl+N"); // confirm
    harness.update(Duration::from_millis(400));
    assert!(
        harness.contains_text(TRUST_QUESTION_SENTINEL),
        "Ctrl+N must not start a session while trust is Pending\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !folder_is_trusted(&content, repo.path()),
        "Ctrl+N must not write a trust grant",
    );

    // Accept => grant persists.
    harness.inject_keys(b"y").expect("inject y");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !folder_is_trusted(&content, repo.path()) && Instant::now() < deadline {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        folder_is_trusted(&content, repo.path()),
        "accepting must persist the trust grant\nscreen:\n{}",
        harness.screen_contents()
    );

    // Session proceeds: typing a prompt now starts a session and streams.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session proceeds after trusting");

    harness.quit().expect("clean quit");
}
