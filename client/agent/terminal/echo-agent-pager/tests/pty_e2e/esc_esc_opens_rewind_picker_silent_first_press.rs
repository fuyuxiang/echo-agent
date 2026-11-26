// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Esc double-press policy (idle, empty prompt, conversation has a user turn):
/// **Esc Esc opens the rewind picker**, and the **first Esc is silent** — no
/// "press again" hint, no toast (unlike the idle clear arm). Proves the
/// rewind arm of `try_handle_esc_policy` (gated on `scrollback.turn_count() > 0`)
/// with a `label: None` silent pending, end-to-end.
///
/// Phase 2 repeats the gesture with the SCROLLBACK pane focused (Tab, footer
/// shows "Space:prompt"): the rewind arm is either-pane, so double-Esc must
/// open the picker from there too — through the scrollback key routing, where
/// the prompt-only regression would hide.
///
/// Uses [`spawn_esc_double_press_pager`] so a slow inter-press round-trip
/// can't expire the arm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn esc_esc_opens_rewind_picker_silent_first_press() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} done."));

    let mut harness = spawn_esc_double_press_pager(&content);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // One real turn → one user-prompt block → turn_count() > 0, so the rewind
    // arm is eligible. The prompt is empty and idle afterwards.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered (has a user turn)");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle");

    // First Esc: arm the rewind picker SILENTLY — no clear/rewind confirm hint.
    // Settle between the presses: a single `ESC ESC` byte pair collapses to
    // one `Esc` in crossterm.
    harness.inject_keys(keys::ESC).expect("first esc");
    harness.update(Duration::from_millis(200));
    let after_first = harness.screen_contents();
    assert!(
        !after_first.contains("press again"),
        "first rewind Esc must be silent (no confirm hint)\nscreen:\n{after_first}"
    );
    assert!(
        !after_first.contains("Rewind to which turn?"),
        "rewind picker must not open until the second Esc\nscreen:\n{after_first}"
    );

    // Second Esc opens the rewind picker (same as /rewind).
    harness.inject_keys(keys::ESC).expect("second esc");
    harness
        .wait_for_text("Rewind to which turn?", Duration::from_secs(15))
        .expect("rewind picker opens on the second Esc");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    // Dismiss the picker (Esc) before the scrollback phase.
    harness.inject_keys(keys::ESC).expect("dismiss rewind");
    harness.update(Duration::from_millis(200));

    // Phase 2: the same double-Esc must arm and open from the SCROLLBACK pane.
    // Single Tab leaves the prompt; the "Space:prompt" footer proves the
    // scrollback owns keys (Tab toggles, so poll the render instead of
    // re-pressing).
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("scrollback must own keys before the rewind Esc");

    harness
        .inject_keys(keys::ESC)
        .expect("first esc (scrollback)");
    harness.update(Duration::from_millis(200));
    let after_first = harness.screen_contents();
    assert!(
        !after_first.contains("press again"),
        "first scrollback rewind Esc must be silent (no confirm hint)\nscreen:\n{after_first}"
    );
    assert!(
        !after_first.contains("Rewind to which turn?"),
        "rewind picker must not open until the second scrollback Esc\nscreen:\n{after_first}"
    );

    harness
        .inject_keys(keys::ESC)
        .expect("second esc (scrollback)");
    harness
        .wait_for_text("Rewind to which turn?", Duration::from_secs(15))
        .expect("rewind picker opens on the second Esc from scrollback");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    // Dismiss the picker (Esc) so the child can exit cleanly.
    harness.inject_keys(keys::ESC).expect("dismiss rewind");
    harness.update(Duration::from_millis(200));

    harness.quit().expect("clean quit");
}
