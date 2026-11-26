// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

// ── Small-screen tip must survive a submit into a slow turn ────────────────
//
// Real-usage repro: the tip shows on the welcome→agent promote, the user hits
// Enter under a second later, and the turn takes several seconds. The submit
// used to retire the tip (`clear_on_submit` before the fix cleared every
// tip), reducing it to a sub-second blink. The scripted scenarios miss this
// because the mock replies instantly; this test paces the mock stream
// (`set_chunk_delay`) so the turn genuinely outlives the ~3s TTL.

/// Exact tip copy (also asserted char-for-char in the unit tests).
const TIP_TEXT: &str = "Tight on space? Try /compact-mode";

/// In the 21..=28 tip band (above the 20-row auto-compact threshold).
const BAND_ROWS: u16 = 24;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn small_screen_tip_survives_slow_turn() {
    let content = ContentController::start().await.expect("start content");
    // ~12 words at 400ms per SSE chunk holds the turn open ~5s — comfortably
    // past the tip's ~3s TTL, like a real inference turn.
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} one two three four five six seven eight nine ten"
    ));
    content.set_chunk_delay(Some(Duration::from_millis(400)));

    let binary = pager_binary().expect("resolve pager binary");
    let env_refs = CONTEXTUAL_HINTS_ENV;
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        BAND_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        env_refs,
    )
    .expect("spawn");

    // The prompt marker paints at every height; the first char promotes the
    // welcome prompt to the agent view, where the tip fires.
    harness
        .wait_for_text("\u{276f}", WELCOME_TIMEOUT)
        .expect("prompt marker");
    harness.inject_keys(b"hi").expect("promote to agent view");
    harness
        .wait_for_text(TIP_TEXT, Duration::from_secs(15))
        .expect("tip shows at the promote");

    // Enter within a second of the show: the turn starts while the tip is up.
    harness.inject_keys(b"\r").expect("submit prompt");
    harness.update(Duration::from_millis(1500));
    let mid_turn = harness.screen_contents();
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited mid-turn\nscreen:\n{mid_turn}"
    );
    assert!(
        mid_turn.contains(TIP_TEXT),
        "tip must stay painted mid-turn (submit must not retire it)\nscreen:\n{mid_turn}"
    );

    // Let the turn finish and the TTL run out: the tip expires exactly once.
    harness
        .wait_for_text("ten", Duration::from_secs(30))
        .expect("slow turn streams to completion");
    wait_for_labels_absent(&mut harness, &[TIP_TEXT], Duration::from_secs(15));
    let settled = harness.screen_contents();
    assert!(
        !settled.contains(TIP_TEXT),
        "tip must expire on its visible TTL\nscreen:\n{settled}"
    );
    assert!(
        !settled.contains("panicked"),
        "pager rendered 'panicked'\nscreen:\n{settled}"
    );

    // No resurrection: later input never brings the one-shot back.
    harness.inject_keys(b"x").expect("type after expiry");
    harness.update(Duration::from_millis(800));
    assert!(
        !harness.contains_text(TIP_TEXT),
        "expired tip must not re-show\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
