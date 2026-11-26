// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal `Ctrl+G` hands the draft to a local non-interactive editor script,
/// restores the native-scrollback live region, and leaves the edited text in
/// the composer until the user explicitly submits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_external_editor_round_trip() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} edited prompt received."));

    let dir = tempfile::tempdir().expect("temp editor dir");
    let editor = fake_editor_command(
        dir.path(),
        "#!/bin/sh\nprintf 'edited draft from external editor' > \"$1\"\n",
        "@echo off\r\n>\"%~1\" echo|set /p=edited draft from external editor\r\n",
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        MINIMAL_ARGS,
        &[EnvOp::set("VISUAL", &editor)],
    )
    .expect("spawn minimal pager");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);
    inject_keys_paced(&mut harness, b"original draft");
    harness.inject_keys(b"\x07").expect("Ctrl+G");

    harness
        .wait_for_text("edited draft from external editor", Duration::from_secs(10))
        .expect("edited draft restored to composer");
    assert!(
        !harness.full_text().contains(MOCK_RESPONSE_SENTINEL),
        "editor exit must not submit the draft"
    );

    harness.inject_keys(b"\r").expect("submit edited draft");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("edited draft submitted");
    let user_messages = all_user_message_blobs(&content);
    assert!(
        user_messages
            .iter()
            .any(|message| message.contains("edited draft from external editor")),
        "exact edited draft must reach the wire: {user_messages:#?}"
    );
    assert!(
        user_messages
            .iter()
            .all(|message| !message.contains("original draft")),
        "original draft must not reach the wire: {user_messages:#?}"
    );
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(10))
        .expect("minimal live region restored idle");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
