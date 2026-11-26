// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A `!` command queued mid-turn is server-promoted after the turn ends; its
/// output must render (the marker file proves execution out-of-band).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
#[cfg(unix)]
async fn queued_bash_promotion_renders_output_pty() {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let step_one = {
        let mut s = String::from("STEPONE");
        for i in 0..150 {
            s.push_str(&format!(" streaming{i}"));
        }
        s
    };
    let _turn_one =
        content.expect_agent_turn("running turn before queued bash promotion", step_one);

    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");
    let cwd = dunce::canonicalize(project.path()).expect("canonicalize project");
    let marker = cwd.join("claim1_marker.txt");

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

    // `CLAIMONE_%s_OK` keeps the executed-output sentinel out of the queue-row text.
    harness
        .inject_keys(b"!printf 'CLAIMONE_%s_OK\\n' RAN | tee claim1_marker.txt\r")
        .expect("submit bash-mode command mid-turn");
    harness
        .wait_for_text("CLAIMONE_%s_OK", Duration::from_secs(10))
        .expect("bash command visible as a queued row");

    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "queued bash never executed server-side (marker missing)\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(200));
    }
    let marker_content = std::fs::read_to_string(&marker).unwrap_or_default();

    let _ = harness.wait_for_text("CLAIMONE_RAN_OK", Duration::from_secs(15));

    assert!(
        harness.contains_text("CLAIMONE_RAN_OK"),
        "SILENT DROP: bash executed server-side (marker {}: {marker_content:?}) \
         but its output never rendered\nscreen:\n{}",
        marker.display(),
        harness.screen_contents()
    );
    harness
        .wait_for_text("Run (user)", Duration::from_secs(15))
        .expect("Run (user) chrome for the drained bash turn");

    let users = all_user_message_blobs(&content);
    assert!(
        !users.iter().any(|u| u.contains("CLAIMONE")),
        "bash command leaked to the model: {users:#?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
