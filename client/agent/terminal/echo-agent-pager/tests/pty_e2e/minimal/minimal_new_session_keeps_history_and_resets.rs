// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// `/new` in minimal mode starts a fresh session — it commits a
/// second welcome card and resets the committed frontier — while leaving the
/// prior turn's committed lines in the terminal's native scrollback (we cannot,
/// and must not, un-print them). After `/new` a fresh turn streams normally.
///
/// The first turn is genuinely taller than the screen (a fenced code block, not
/// reflowable prose) so the first welcome card and the turn's head scroll into
/// *native scrollback* before `/new`. The robust "new session" signal is then
/// that a *second* welcome card appears (two `EchoAgent Build` banners in
/// scrollback+screen), independent of exactly how content scrolled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_new_session_keeps_history_and_resets() {
    /// Substring printed once per minimal welcome card (see `minimal::welcome`).
    const WELCOME_BANNER: &str = "EchoAgent Build";

    let content = ContentController::start().await.expect("start content");
    // Code-block rows (not prose, which markdown-reflows to fit on screen) so
    // turn 1 is genuinely taller than the screen and its head commits into
    // native scrollback before `/new` — see `tall_response`.
    content.set_response(tall_response(&turn_sentinel(1), 80));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 1");

    // Wait until turn 1's head has committed into *native scrollback* (above the
    // viewport). The first welcome card was printed before it, so it is in
    // scrollback too by this point.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && !harness.scrollback_text().contains(&turn_sentinel(1)) {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness.scrollback_text().contains(&turn_sentinel(1)),
        "turn 1 must reach native scrollback before /new\nscrollback:\n{}",
        harness.scrollback_text()
    );

    // `/new` → fresh session: commits a second welcome card and resets the frontier.
    inject_keys_paced(&mut harness, b"/new");
    harness.inject_keys(b"\r").expect("submit /new");

    // The "new session" signal: a *second* welcome banner now exists across
    // scrollback + screen (the first is preserved in native scrollback).
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && harness.full_text().matches(WELCOME_BANNER).count() < 2 {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        harness.full_text().matches(WELCOME_BANNER).count() >= 2,
        "/new must commit a second welcome card (first preserved in scrollback)\nfull:\n{}",
        harness.full_text()
    );

    // Prior turn's committed lines remain in native scrollback (not wiped).
    assert!(
        harness.contains_full_text(&turn_sentinel(1)),
        "prior turn must remain in native scrollback after /new\nfull:\n{}",
        harness.full_text()
    );

    // A fresh turn streams in the new session.
    content.set_response(format!("{} new session payload.", turn_sentinel(2)));
    harness
        .inject_keys(b"hi\r")
        .expect("submit a turn in the new session");
    harness
        .wait_for_full_text(&turn_sentinel(2), Duration::from_secs(30))
        .expect("new-session turn streams");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
