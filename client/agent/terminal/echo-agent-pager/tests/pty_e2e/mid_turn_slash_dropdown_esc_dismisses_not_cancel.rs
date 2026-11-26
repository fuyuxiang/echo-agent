// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Overlay-steal precedence: while a turn is streaming, opening the slash
/// dropdown and pressing **Esc dismisses the dropdown and does NOT cancel the
/// turn** (it never reaches the mid-turn Esc policy). The pane-level slash
/// handler returns `Changed` before `try_handle_esc_policy` ever runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mid_turn_slash_dropdown_esc_dismisses_not_cancel() {
    let content = ContentController::start().await.expect("start content");
    let long_response = format!(
        "{MOCK_RESPONSE_SENTINEL} {}",
        "streaming filler words while the dropdown is open. ".repeat(120)
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

    // Open the slash dropdown mid-turn. "/mod" narrows to `/model`, whose
    // description renders only in the dropdown (not in the typed text).
    inject_keys_paced(&mut harness, b"/mod");
    harness
        .wait_for_text("Switch the active model", Duration::from_secs(10))
        .expect("slash dropdown open mid-turn");

    // Esc: dropdown steals it (dismiss), the turn must keep running.
    harness.inject_keys(keys::ESC).expect("press esc");
    harness.update(Duration::from_millis(400));

    let screen = harness.screen_contents();
    assert!(
        !screen.contains("Switch the active model"),
        "Esc must dismiss the slash dropdown\nscreen:\n{screen}"
    );
    assert!(
        !screen.contains("Turn cancelled by user"),
        "slash-dropdown Esc must NOT cancel the turn\nscreen:\n{screen}"
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{screen}"
    );

    // Settle a little more and re-confirm the turn was never cancelled by the
    // dropdown-dismiss Esc.
    harness.update(Duration::from_millis(600));
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "turn must still be running after the dropdown-dismiss Esc\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
