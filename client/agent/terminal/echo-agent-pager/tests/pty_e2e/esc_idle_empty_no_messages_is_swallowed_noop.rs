// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Esc policy (idle, empty prompt, NO user turns): **Esc is a swallowed
/// no-op** — it must NOT focus the scrollback (the pre-port behavior) and must
/// not panic or arm anything. Guards the `try_handle_esc_policy` final
/// `Some(InputOutcome::Changed)` swallow branch on a fresh session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn esc_idle_empty_no_messages_is_swallowed_noop() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} unused."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Promote the welcome screen into a real (idle) agent session by typing,
    // then wipe the draft so the prompt is empty with NO submitted user turn.
    harness
        .inject_keys(b"NOMSG")
        .expect("type to promote session");
    harness
        .wait_for_text("NOMSG", Duration::from_secs(10))
        .expect("draft renders in the promoted agent prompt");
    harness.inject_keys(b"\x15").expect("Ctrl+U clear to empty");
    wait_for_labels_absent(&mut harness, &["NOMSG"], Duration::from_secs(5));

    // Baseline: the prompt owns keys (scrollback's "Space:prompt" hint absent).
    assert!(
        !harness.contains_text("Space:prompt"),
        "precondition: prompt should be focused before Esc\nscreen:\n{}",
        harness.screen_contents()
    );

    // Esc TWICE on idle + empty + no-messages: a true swallow stays a no-op on
    // the second press. Pressing once and typing couldn't distinguish a swallow
    // from a silently-armed rewind (the next keystroke clears any pending); a
    // wrongly-armed rewind would instead OPEN the picker on the second Esc.
    harness.inject_keys(keys::ESC).expect("press esc (1)");
    harness.update(Duration::from_millis(250));
    harness.inject_keys(keys::ESC).expect("press esc (2)");
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    assert!(
        !screen.contains("Space:prompt"),
        "idle empty Esc must be swallowed, NOT focus scrollback\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("press again"),
        "idle empty Esc must not arm any double-press\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("Rewind to which turn?"),
        "Esc Esc with no messages must NOT open the rewind picker\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{screen}"
    );

    // The prompt is still live: typing lands in the composer.
    harness.inject_keys(b"STILLHERE").expect("type after esc");
    harness
        .wait_for_text("STILLHERE", Duration::from_secs(10))
        .expect("prompt still accepts input after the no-op Esc");

    harness.quit().expect("clean quit");
}
