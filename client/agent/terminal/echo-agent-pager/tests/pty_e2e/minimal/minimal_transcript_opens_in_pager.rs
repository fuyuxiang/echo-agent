// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal full view: `/transcript` renders the WHOLE conversation
/// fully expanded (reasoning in full, tool output uncapped) as ANSI to a temp
/// file and opens it in `$PAGER`, suspending the inline TUI, then restores. We
/// set `PAGER=cat` so the child dumps the transcript and exits immediately (no
/// interactive `less` to drive). Proof the pager ran on the transcript: the
/// turn's sentinel then appears **twice** — once in the live conversation and
/// once in the dumped transcript — and the inline TUI restores to idle after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_transcript_opens_in_pager() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} transcript body."));

    // Minimal env + PAGER=cat (non-interactive). Response forwarding on so the
    // inline-viewport cursor probe completes (see spawn_minimal).
    let overrides: Vec<(String, String)> = vec![("PAGER".to_string(), "cat".to_string())];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        MINIMAL_ARGS,
        &env_refs,
    )
    .expect("spawn minimal pager");
    harness.set_respond_to_queries(true);

    wait_minimal_ready(&mut harness);

    // Produce a turn so there is something to transcribe.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn committed");

    // Open the transcript in $PAGER (cat dumps it and exits).
    inject_keys_paced(&mut harness, b"/transcript");
    harness.inject_keys(b"\r").expect("submit /transcript");

    // The pager (cat) dumps the full transcript, which re-emits the turn body,
    // so the sentinel now appears at least twice across scrollback + screen (the
    // live turn plus the dumped transcript). This proves the pager ran on the
    // rendered transcript rather than the command being a no-op.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline
        && harness.full_text().matches(MOCK_RESPONSE_SENTINEL).count() < 2
    {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness.full_text().matches(MOCK_RESPONSE_SENTINEL).count() >= 2,
        "transcript must be dumped by $PAGER (sentinel should appear in both the \
         live turn and the dumped transcript)\nfull:\n{}",
        harness.full_text()
    );

    // The pager process survives the suspend/restore round-trip and returns to
    // the idle prompt.
    harness
        .wait_for_text(MINIMAL_IDLE_SENTINEL, Duration::from_secs(10))
        .expect("inline TUI restored after the pager exited");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
