// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Paste-then-send race guard, end to end: a bracketed paste with Enter
/// arriving in the SAME input burst (terminal auto-paste-and-run, fast users)
/// must submit the full payload exactly ONCE — never dropped, never
/// duplicated.
///
/// What each platform actually exercises: on Linux the paste path is fully
/// synchronous (the clipboard probe block is cfg(macos/windows)), so this
/// guards the plain paste→submit ordering. On macOS the deferred-probe send
/// stash is exercised ONLY when the real host pasteboard happens to carry a
/// raster (the snapshot gate skips the probe otherwise); the test stays
/// hermetic and does not seed one — the stash itself is covered by unit tests
/// (`agent_send_before_paste_probe_keeps_image`).
///
/// "Exactly once" is asserted on user messages, not HTTP requests: the shell
/// may legitimately retry the SAME turn over a second endpoint (Responses →
/// Chat Completions fallback against the mock), but a dropped send leaves 0
/// payload-bearing user messages and a double-submit leaves 2 in one
/// request's accumulated history.
///
/// On a macOS dev machine the real host clipboard may add an incidental image
/// chip, so asserts are contains-style on sentinels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn paste_bracketed_then_immediate_enter_sends_intact() {
    const LINE_1: &str = "ZRACELINEONE pasted body first";
    const LINE_2: &str = "ZRACELINETWO pasted body second";
    const LINE_3: &str = "ZRACELINETHREE pasted body third";

    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} paste race turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Paste + Enter in ONE injected buffer: the Enter chases the closing
    // ESC[201~ with no settle, submitting before any deferred paste work
    // could possibly finish.
    harness
        .inject_keys(format!("\x1b[200~{LINE_1}\n{LINE_2}\n{LINE_3}\x1b[201~\r").as_bytes())
        .expect("bracketed paste + immediate Enter");

    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response for the pasted-then-sent prompt");

    // Settle so any stray follow-up request would have landed before counting.
    harness.update(Duration::from_secs(2));

    // Serialized-body counting stays agnostic to the endpoint shape
    // (`messages` for Chat Completions, `input` for Responses).
    let per_body_payload_counts: Vec<usize> = content
        .request_bodies()
        .iter()
        .map(|b| b.to_string().match_indices("ZRACELINEONE").count())
        .collect();
    assert!(
        per_body_payload_counts.contains(&1),
        "paste + immediate Enter must submit the payload (dropped send?); \
         per-body payload counts: {per_body_payload_counts:?}, requests: {:?}",
        content
            .requests()
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        per_body_payload_counts.iter().all(|&c| c <= 1),
        "paste + immediate Enter must not double-submit the payload; \
         per-body payload counts: {per_body_payload_counts:?}\nuser blobs: {:?}",
        all_user_message_blobs(&content)
    );

    let blobs = all_user_message_blobs(&content);
    assert!(
        blobs
            .iter()
            .any(|m| m.contains(LINE_1) && m.contains(LINE_2) && m.contains(LINE_3)),
        "the submitted user message must carry all three pasted sentinel lines; got {blobs:?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
