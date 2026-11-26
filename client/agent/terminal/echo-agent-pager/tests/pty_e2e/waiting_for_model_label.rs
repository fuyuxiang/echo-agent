// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The turn-status spinner names *what* the agent is waiting on. Right after a
/// prompt is submitted, before the model streams its first token, the spinner
/// reads "Waiting for response…" — the explicit `WaitingReason::Model` label
/// that replaced the old opaque "Waiting…". This is the literal feedback that
/// prompted the change ("what is 'Waiting…' waiting on?").
///
/// Drives the real binary end-to-end: prompt submit → `resolve_turn_activity`
/// (no streamed activity, not bash, no subagent → `Waiting(Model)`) →
/// `compute_activity` → rendered status line. A 3s per-event mock delay holds
/// the pre-first-token window open long enough to observe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn waiting_for_model_label_shows_before_first_token() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} done."));
    // Every SSE event (including the first) is emitted after this delay, so the
    // turn sits in the "no activity yet" state — Waiting(Model) — for ~3s after
    // submit, before the first response chunk arrives.
    content.set_chunk_delay(Some(Duration::from_secs(3)));

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

    // The new explicit label, not the old generic "Waiting…". Match without the
    // trailing ellipsis so terminal width / glyph handling can't flake it.
    harness
        .wait_for_text("Waiting for response", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "expected 'Waiting for response…' spinner before first token\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // Let the turn finish so the quit is clean and we prove the wait resolves.
    content.set_chunk_delay(None);
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("response streamed after the wait");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
