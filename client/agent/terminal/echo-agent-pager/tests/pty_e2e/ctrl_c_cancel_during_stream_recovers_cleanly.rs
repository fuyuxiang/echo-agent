// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 17. **Ctrl+C-cancel mid-stream recovers cleanly.**
///
/// Cancelling a streaming turn must (a) surface the "Turn cancelled" marker
/// exactly once — the turn end now arrives via BOTH the PromptResponse RPC
/// and the `prompt_complete` broadcast (which arms the lost-response
/// reconcile), and a double-finish would render two markers — and (b) leave
/// the pane usable: no `TurnCancelling` latch, the next typed prompt runs.
/// Cancel is via Ctrl+C, which works in every mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ctrl_c_cancel_during_stream_recovers_cleanly() {
    let content = ContentController::start().await.expect("start content");
    // A long, paced response keeps the turn visibly streaming while Ctrl+C is
    // pressed (the mock otherwise dumps all SSE events instantly and the
    // turn completes before the cancel lands).
    let long_response = format!(
        "{MOCK_RESPONSE_SENTINEL} {}",
        "streaming filler words for the cancellation window. ".repeat(120)
    );
    content.set_response(long_response);
    content.set_chunk_delay(Some(Duration::from_millis(50)));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("stream started");

    // Ctrl+C on an empty prompt cancels while streaming.
    harness.inject_keys(keys::CTRL_C).expect("press ctrl+c");
    harness.update(Duration::from_millis(200));

    harness
        .wait_for_text("Turn cancelled by user", Duration::from_secs(15))
        .expect("turn cancelled marker");

    // Settle, then assert the marker rendered exactly once — a double marker
    // means the broadcast reconcile double-finished the turn alongside the
    // PromptResponse path.
    harness.update(Duration::from_millis(1000));
    let screen = harness.screen_contents();
    assert_eq!(
        screen.matches("Turn cancelled by user").count(),
        1,
        "'Turn cancelled' must appear exactly once\nscreen:\n{screen}"
    );

    // Recovery: the pane must accept and run a new prompt (no
    // TurnCancelling latch, which previously required a restart here).
    content.set_chunk_delay(None);
    content.set_response("RECOVERYSENTINEL post-cancel turn ran.");
    harness
        .inject_keys(b"run again\r")
        .expect("submit post-cancel prompt");
    harness
        .wait_for_text("RECOVERYSENTINEL", Duration::from_secs(30))
        .expect("post-cancel turn streamed");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");

    // The new cancel-path observability must have recorded the cancel
    // end-to-end in the (isolated) unified log: the agent received the
    // `session/cancel` notification and processed it against the running
    // prompt. Absence of these markers was exactly what made the original
    // incident undiagnosable.
    let unified_log = content
        .home()
        .join(".echo-agent")
        .join("logs")
        .join("unified.jsonl");
    let unified = std::fs::read_to_string(&unified_log).unwrap_or_default();
    assert!(
        unified.contains("\"msg\":\"shell.cancel.received\""),
        "unified log must record the agent receiving the cancel\nlog path: {}",
        unified_log.display()
    );
    assert!(
        unified.contains("\"msg\":\"shell.cancel.processing\""),
        "unified log must record the cancel being processed\nlog path: {}",
        unified_log.display()
    );
}
