// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
use echo_agent_workspace::trust::{TRUST_FILE_NAME, TrustStore};

/// Folder-trust home-is-a-git-repo (dotfiles-in-home), Case 1 — the reported bug.
/// `$HOME` is itself a git repo; the session is launched in a SUBDIR
/// (`<home>/proj`) that has its own repo-local `.mcp.json`. The trust question
/// must render for — and the accepted grant must persist keyed on — the SUBDIR,
/// NEVER on `$HOME` (the bug resolved the prompt/key up to `$HOME` because the
/// git up-walk landed on the home repo root).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn folder_trust_home_git_repo_subdir_keys_on_subdir() {
    let content = ContentController::start().await.expect("start content");

    // $HOME is a dotfiles-style git repo; the launch dir is a subdir carrying its
    // own repo-local code-exec config (so the SUBDIR has something to gate).
    git2::Repository::init(content.home()).expect("git init $HOME");
    let proj = content.home().join("proj");
    std::fs::create_dir_all(&proj).expect("create proj subdir");
    std::fs::write(proj.join(".mcp.json"), "{}").expect("write proj/.mcp.json");

    let env_refs = trust_env(true);
    let cwd = proj.to_str().expect("utf8 proj path");

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

    // Check the trust store DIRECTLY, not via `folder_is_trusted` (which
    // re-derives `workspace_key` in the TEST process, whose `$HOME` is NOT
    // `content.home()`, so its home guard wouldn't fire and it would resolve the
    // wrong key). `TrustStore::is_trusted` canonicalizes + ancestor-prefix matches
    // internally, so passing the raw path is HOME-independent.
    let store_path = content.home().join(".echo-agent").join(TRUST_FILE_NAME);

    // The question renders (keyed on the subdir), and the store is empty first.
    harness
        .wait_for_text(TRUST_QUESTION_SENTINEL, WELCOME_TIMEOUT)
        .expect("trust question renders for the subdir");
    assert!(
        !TrustStore::load_from(store_path.clone()).is_trusted(&proj),
        "store must be empty before the user answers",
    );

    // Accept => the grant persists, trusting the SUBDIR. The child writes the
    // store async after `y`, so reload it each poll iteration.
    harness.inject_keys(b"y").expect("inject y");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut trusted = false;
    while Instant::now() < deadline {
        if TrustStore::load_from(store_path.clone()).is_trusted(&proj) {
            trusted = true;
            break;
        }
        harness.update(Duration::from_millis(100));
    }
    assert!(
        trusted,
        "accepting must persist a grant that trusts the subdir\nscreen:\n{}",
        harness.screen_contents()
    );

    // Core regression: the grant must NOT trust $HOME (the reported bug keyed on $HOME).
    assert!(
        !TrustStore::load_from(store_path).is_trusted(content.home()),
        "trust must key on the subdir, never on $HOME",
    );

    harness.quit().expect("clean quit");
}
