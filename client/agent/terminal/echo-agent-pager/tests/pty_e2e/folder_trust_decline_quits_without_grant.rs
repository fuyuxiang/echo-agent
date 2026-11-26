// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 14. **Folder-trust decline quits the pager (no grant).**
/// Same setup; pressing `n` exits the pager (the process ends) and writes NO
/// grant — the product decision is decline => quit, not proceed-gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn folder_trust_decline_quits_without_grant() {
    let content = ContentController::start().await.expect("start content");
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

    harness
        .wait_for_text(TRUST_QUESTION_SENTINEL, WELCOME_TIMEOUT)
        .expect("trust question renders");

    // Decline => the pager quits (no session, no grant).
    harness.inject_keys(b"n").expect("inject n");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !harness.is_running().expect("poll pager liveness") {
            break;
        }
        harness.update(Duration::from_millis(100));
    }
    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        !running,
        "declining the trust question must quit the pager\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !folder_is_trusted(&content, repo.path()),
        "declining must NOT persist a grant",
    );
}
