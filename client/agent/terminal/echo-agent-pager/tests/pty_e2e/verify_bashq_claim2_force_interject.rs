// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The queue-pane (Ctrl+Enter) arm of send-now on a bash row: the selected
/// `!` row is promoted to run NOW as its own bash turn (silent cancel of the
/// running turn) — it must execute as real bash, never reaching the model as
/// prompt text and never rendering a "❯ !…" block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[cfg(unix)]
async fn verify_bashq_claim2_force_interject() {
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
    let _unexpected_turn = content.expect_agent_turn(
        "unexpected model continuation for bash send-now",
        "STEPTWO force-send continuation.",
    );

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

    // `CLAIMTWO_%s_OK` keeps the output sentinel out of the queue-row text.
    harness
        .inject_keys(b"!printf 'CLAIMTWO_%s_OK\\n' FS\r")
        .expect("submit bash-mode command mid-turn");
    harness
        .wait_for_text("CLAIMTWO_%s_OK", Duration::from_secs(10))
        .expect("bash command visible as a queued row");

    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("focus queue pane");
    harness.update(Duration::from_millis(300));
    harness
        .inject_keys(CTRL_ENTER)
        .expect("send the selected bash row now");

    // Send-now: the row executes as real bash NOW, as its own turn.
    harness
        .wait_for_text("CLAIMTWO_FS_OK", Duration::from_secs(30))
        .expect("queued bash executed via send-now");
    harness
        .wait_for_text("Run (user)", Duration::from_secs(15))
        .expect("Run (user) chrome for the promoted bash turn");

    let users = all_user_message_blobs(&content);
    assert!(
        !users.iter().any(|u| u.contains("CLAIMTWO")),
        "FORCE-SEND CORRUPTION: bash command reached the model as prompt text: {users:#?}"
    );
    // A row wrongly sent as a model prompt would commit a "❯ !printf…" block
    // and consume the STEPTWO continuation.
    assert!(
        !harness.contains_text("\u{276F} !printf"),
        "bash row must not render a user-prompt block\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("STEPTWO"),
        "no model continuation may run for a bash send-now\nscreen:\n{}",
        harness.screen_contents()
    );
    // The send-now cancel of turn 1 is silent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
