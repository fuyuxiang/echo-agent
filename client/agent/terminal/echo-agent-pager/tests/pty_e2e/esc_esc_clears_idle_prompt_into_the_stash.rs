// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Esc double-press policy (idle, non-empty prompt): the **first Esc shows
/// "press again to clear"** and the **second Esc clears the prompt** into the
/// stash, which the border caption reports. The draft was never sent, so the
/// Up-arrow history panel must not list it. Proves `try_handle_esc_policy`'s
/// idle clear arm + `dispatch_clear_prompt` end-to-end on the real binary.
///
/// Uses [`spawn_esc_double_press_pager`] so a slow inter-press round-trip
/// can't expire the arm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn esc_esc_clears_idle_prompt_into_the_stash() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} done."));

    let mut harness = spawn_esc_double_press_pager(&content);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Establish a real idle session first (so Esc runs the agent policy, not
    // welcome-screen handling), then type a fresh draft to clear.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("first turn rendered");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle");

    let draft = "ZZCLEARDRAFT";
    harness.inject_keys(draft.as_bytes()).expect("type draft");
    harness
        .wait_for_text(draft, Duration::from_secs(10))
        .expect("draft renders in the composer");

    // Wait for the confirm hint between the presses: it proves the arm landed,
    // and a single `ESC ESC` byte pair collapses to one `Esc` in crossterm.
    harness.inject_keys(keys::ESC).expect("first esc");
    harness
        .wait_for_text("press again to clear", Duration::from_secs(15))
        .expect("first idle Esc must show the clear confirm hint");

    // Second Esc fires the clear.
    harness.inject_keys(keys::ESC).expect("second esc");
    wait_for_labels_absent(&mut harness, &[draft], Duration::from_secs(5));
    assert!(
        !harness.contains_text(draft),
        "second Esc must clear the draft\nscreen:\n{}",
        harness.screen_contents()
    );
    // The confirm hint must be gone once the pending fired.
    assert!(
        !harness.contains_text("press again to clear"),
        "clear-confirm hint must clear after the second Esc fires\nscreen:\n{}",
        harness.screen_contents()
    );

    // The cleared text went to the stash, which the prompt border reports.
    harness
        .wait_for_text("Stashed", Duration::from_secs(10))
        .expect("the clear must stash the draft");

    // The stash is not a history source: Up on the now-empty prompt opens the
    // history panel, and the unsent draft must not be in it.
    harness.inject_keys(keys::UP).expect("open history panel");
    assert!(
        harness
            .wait_for_text(draft, Duration::from_secs(5))
            .is_err(),
        "a stashed draft must not be recallable from the history\nscreen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
