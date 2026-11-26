// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Resuming a minimal session with `--continue` reprints
/// the prior transcript into native scrollback. Minimal has no separate history
/// pane (the terminal owns history), so a resumed session would otherwise look
/// empty — rather than a compact resume marker, minimal reprints the full
/// transcript instead. This asserts the prior
/// turn's content reappears after resume and a follow-up turn still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_continue_reprints_transcript() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{} first session payload.", turn_sentinel(1)));

    // Sessions are keyed by cwd: both runs must share a stable project dir.
    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let mut first = spawn_minimal_in_dir(&content, DEFAULT_ROWS, DEFAULT_COLS, &[], project.path());
    wait_minimal_ready(&mut first);
    first
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 1");
    first
        .wait_for_full_text(&turn_sentinel(1), Duration::from_secs(30))
        .expect("turn 1 committed to scrollback");
    // Idle before quit so the agent finishes turn completion + updates.jsonl
    // flush. Quitting mid-finalize under suite load left `--continue` loading a
    // session with the user message but no assistant payload (resume then
    // shows "Loading session…" / empty chrome past RESUME_TIMEOUT).
    first
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(15))
        .expect("turn 1 returned to idle before quit");
    first.update(Duration::from_millis(300));
    quit_minimal(&mut first);

    // Resume the same session. The transcript is reprinted into native
    // scrollback (no separate history pane in minimal).
    let mut resumed = spawn_minimal_in_dir(
        &content,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &["--continue"],
        project.path(),
    );
    resumed
        .wait_for_full_text(&turn_sentinel(1), RESUME_TIMEOUT)
        .unwrap_or_else(|e| {
            panic!(
                "history must be reprinted after --continue: {e}\nfull:\n{}",
                resumed.full_text()
            )
        });

    // A follow-up turn still works in the resumed session.
    content.set_response(format!("{} resumed payload.", turn_sentinel(2)));
    resumed
        .inject_keys(b"again\r")
        .expect("submit turn 2 after resume");
    resumed
        .wait_for_full_text(&turn_sentinel(2), Duration::from_secs(30))
        .expect("turn 2 rendered in resumed session");

    assert!(
        !resumed.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        resumed.screen_contents()
    );

    quit_minimal(&mut resumed);
}
