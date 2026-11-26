// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 16. **`--continue` resumes the latest session.**
/// History must render exactly once (duplicate replay and empty pane both
/// fail) and the resumed session must accept a follow-up turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn continue_resumes_session_with_history() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{} first session payload.", turn_sentinel(1)));

    // Sessions are keyed by cwd: both runs must share a stable project dir.
    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let binary = pager_binary().expect("resolve pager binary");
    let mut first = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        Some(project.path()),
    )
    .expect("spawn first pager");

    first
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    first
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 1");
    first
        .wait_for_text(&turn_sentinel(1), Duration::from_secs(30))
        .expect("turn 1 rendered");
    // Quit via Ctrl+Q double-press: focus is in the prompt, so 'q' would just type.
    first.update(Duration::from_millis(500));
    first.inject_keys(b"\x11").expect("ctrl-q once");
    first.update(Duration::from_millis(200));
    first.inject_keys(b"\x11").expect("ctrl-q confirm");
    first.quit().expect("reap first pager");

    let mut resumed = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--continue"],
        Some(project.path()),
    )
    .expect("spawn resumed pager");

    resumed
        .wait_for_text(&turn_sentinel(1), WELCOME_TIMEOUT)
        .expect("history replayed after --continue");
    let screen = resumed.screen_contents();
    assert_eq!(
        screen.matches(&turn_sentinel(1)).count(),
        1,
        "turn 1 must appear exactly once after resume\nscreen:\n{screen}"
    );

    content.set_response(format!("{} resumed session payload.", turn_sentinel(2)));
    resumed
        .inject_keys(b"again\r")
        .expect("submit turn 2 after resume");
    resumed
        .wait_for_text(&turn_sentinel(2), Duration::from_secs(30))
        .expect("turn 2 rendered in resumed session");

    assert!(
        !resumed.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        resumed.screen_contents()
    );

    resumed.quit().expect("quit resumed pager");
}
