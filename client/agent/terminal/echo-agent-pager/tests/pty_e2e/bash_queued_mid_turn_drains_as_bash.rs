// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A `!` row queued mid-turn stays real bash under empty-Enter send-now:
/// the row is promoted to run NOW as its own bash turn (silent cancel of the
/// running turn — no "Turn cancelled by user" marker), never leaking to the
/// model as prompt/interjection text and never rendering a "❯ !…" block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[cfg(unix)]
async fn bash_queued_mid_turn_drains_as_bash() {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let step_one = {
        let mut s = String::from("STEPONE");
        for i in 0..150 {
            s.push_str(&format!(" streaming{i}"));
        }
        s
    };
    let _turn_one = content.expect_agent_turn("running turn before queued bash send-now", step_one);

    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");
    let cwd = dunce::canonicalize(project.path()).expect("canonicalize project");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        Some(cwd.as_path()),
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("STEPONE", Duration::from_secs(30))
        .expect("turn 1 streaming");

    // `QBASH_%s_OK` keeps the output sentinel out of the queue-row text.
    harness
        .inject_keys(b"!printf 'QBASH_%s_OK\\n' MIDTURN\r")
        .expect("submit bash-mode command mid-turn");
    harness
        .wait_for_text("QBASH_%s_OK", Duration::from_secs(10))
        .expect("bash command visible as a queued row");

    // Empty Enter is send-now (cancel-and-send): the shell silently cancels
    // turn 1 and runs the bash row as its own next turn immediately.
    harness.inject_keys(b"\r").expect("empty Enter send-now");
    harness
        .wait_for_text("QBASH_MIDTURN_OK", Duration::from_secs(30))
        .expect("queued bash command executed now (send-now)");
    harness
        .wait_for_text("Run (user)", Duration::from_secs(15))
        .expect("Run (user) chrome for the promoted bash turn");

    // Bash rows never render a user-prompt block (the execute block IS the
    // visual entry) — a "❯ !printf…" block would mean the row went to the
    // model as text instead of executing.
    assert!(
        !harness.contains_text("\u{276F} !printf"),
        "bash row must not render a user-prompt block\nscreen:\n{}",
        harness.screen_contents()
    );
    // The send-now cancel of turn 1 is silent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    let users = all_user_message_blobs(&content);
    assert!(
        !users.iter().any(|u| u.contains("QBASH")),
        "bash command leaked to the model as a prompt/interjection: {users:#?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
