// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 22. **Send-now delivery vs. explicit cancel.** (Historical name: the chord
/// used to buffer a same-turn interjection that a cancel could discard.)
/// The chord is now cancel-and-send: text + Ctrl+Enter mid-stream silently
/// cancels the running turn and delivers the text as its OWN next turn (with
/// the interjection preamble). A later explicit Ctrl+C still renders
/// its "Turn cancelled by user" marker — the consumed send-now expectation
/// must never suppress a real user cancel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn cancel_discards_buffered_interjection() {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let _cancelled_turn =
        content.expect_agent_turn("turn cancelled by send-now", slow_turn_text("CANCELTURN"));
    let _explicitly_cancelled_turn = content.expect_agent_turn(
        "send-now turn cancelled explicitly",
        slow_turn_text("STEERTURN"),
    );
    let _fresh_turn = content.expect_agent_turn(
        "fresh turn after explicit cancel",
        "FRESHTURN after cancel.",
    );

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
        .wait_for_text("CANCELTURN", Duration::from_secs(30))
        .expect("turn streaming");

    harness
        .inject_keys(b"deliver this steer now")
        .expect("type steering");
    harness.inject_keys(CTRL_ENTER).expect("send-now chord");
    // Cancel-and-send: the steer commits as a standard "❯ " prompt block via
    // the turn-start adoption and runs as its own turn.
    harness
        .wait_for_text("\u{276F} deliver this steer now", Duration::from_secs(15))
        .expect("send-now prompt block");
    harness
        .wait_for_text("STEERTURN", Duration::from_secs(30))
        .expect("steer runs as its own turn");
    // The send-now half cancelled turn 1 silently.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    // Explicit Ctrl+C on the steer turn (still streaming): a REAL cancel,
    // whose marker must render — the earlier send-now expectation was
    // consumed and must not silence it.
    harness.inject_keys(keys::CTRL_C).expect("cancel turn");
    harness
        .wait_for_text("Turn cancelled by user", Duration::from_secs(10))
        .expect("explicit cancel marker");

    harness
        .inject_keys(b"fresh prompt\r")
        .expect("submit fresh prompt");
    harness
        .wait_for_text("FRESHTURN", Duration::from_secs(30))
        .expect("fresh turn response");

    let users = all_user_message_blobs(&content);
    let steers: Vec<_> = users
        .iter()
        .filter(|u| u.contains("deliver this steer now"))
        .collect();
    assert!(
        !steers.is_empty(),
        "send-now steer never reached the wire: {users:#?}"
    );
    for s in &steers {
        assert!(
            s.contains(INTERJECTION_WIRE_PREFIX),
            "send-now must use the interjection preamble: {s}"
        );
        assert!(
            s.contains("<user_query>"),
            "send-now must wrap the steered text in user_query: {s}"
        );
    }

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
