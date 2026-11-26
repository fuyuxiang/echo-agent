// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

use std::time::Duration;

// ── A8: scroll-debug HUD e2e (`ECHO_AGENT_SCROLL_DEBUG`) ─────────────────────────
//
// The HUD is a release-compiled overlay gated purely at runtime, so the
// stock harness binary must (a) show it when the env var is set and (b) show
// nothing without it. Determinism notes:
//
// - `ECHO_AGENT_SCROLL_MODE=trackpad` pins classification (the A5 env → live
//   config path): EVERY finalized stream records a `last:trackpad`
//   breadcrumb no matter how CI jitter splits the burst, so asserting that
//   exact HUD text is timing-safe. `mode:trackpad` additionally witnesses
//   the config echo end-to-end.
// - The pager repaints only when scroll lines land (the scroll clock's
//   zero-line finalize does not draw), so after the flood settles a single
//   follow-up notch forces a fresh frame: its first-event flush always
//   applies >= 1 line (trackpad prices one event at a whole line at default
//   speed), and that frame paints the flood's finalized breadcrumb.

/// 120 one-row markers >> the 50-row PTY: early markers sit off-screen-top.
const MARKER_COUNT: usize = 120;

/// **Env-on e2e.** `ECHO_AGENT_SCROLL_DEBUG=1` must paint the HUD (panel title +
/// config echo) and track a finalized trackpad flood, without eating the
/// scroll input itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scroll_debug_hud_env_shows_hud_and_tracks_flood() {
    let (mut harness, _content, top_before) = spawn_bottom_pinned_marker_scrollback_with_env(
        MARKER_COUNT,
        &[
            ("ECHO_AGENT_SCROLL_DEBUG", "1"),
            ("ECHO_AGENT_SCROLL_MODE", "trackpad"),
        ],
    )
    .await;

    // The primed screen already painted frames — the HUD is on before any
    // scrolling, echoing the env-forced mode.
    assert!(
        harness.contains_text("scroll debug"),
        "HUD title missing with ECHO_AGENT_SCROLL_DEBUG=1\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("mode:trackpad"),
        "HUD must echo the env-forced scroll mode\nscreen:\n{}",
        harness.screen_contents()
    );

    // Trackpad-paced flood, then settle past the 80ms gap so it finalizes.
    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        30,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::from_millis(8),
    );
    harness.update(Duration::from_millis(600));

    // One more notch forces a post-finalize repaint (see header note); the
    // frame it paints carries the flood's breadcrumb.
    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        1,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    harness.update(Duration::from_millis(300));

    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        running && !harness.contains_text("panicked"),
        "pager broke during the HUD flood\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        harness.contains_text("last:trackpad"),
        "HUD must record the finalized stream's kind\nscreen:\n{}",
        harness.screen_contents()
    );

    // The HUD is an observer: the flood must still have scrolled the pane.
    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the flood\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "wheel-up flood did not scroll with the HUD on: topmost marker \
         {} → {}\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

/// **Default-off e2e.** Without the env var no HUD text may appear.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn scroll_debug_hud_absent_without_env() {
    let (mut harness, _content, _top) = spawn_bottom_pinned_marker_scrollback(MARKER_COUNT).await;

    assert!(
        !harness.contains_text("scroll debug"),
        "HUD must stay dark without ECHO_AGENT_SCROLL_DEBUG\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

// ── A10: `/debug scroll` e2e ────────────────────────────────────────────────

/// **Command-toggle e2e.** `/debug scroll` typed in the prompt must flip the
/// HUD on without the env var, and a second invocation must flip it off.
/// Works against debug AND release harness binaries: `/debug` is registered
/// everywhere (only its dropdown listing is debug-gated), and Enter with the
/// dropdown open accepts the exact-match arg then sends — same submit either
/// way. Leading text is `trim()`ed by dispatch, so the Space that refocuses
/// the prompt is harmless even if it were to land as a character.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn debug_scroll_command_toggles_hud_live() {
    let (mut harness, _content, _top) = spawn_bottom_pinned_marker_scrollback(MARKER_COUNT).await;

    assert!(
        !harness.contains_text("scroll debug"),
        "HUD must start dark without ECHO_AGENT_SCROLL_DEBUG\nscreen:\n{}",
        harness.screen_contents()
    );

    // The spawn helper leaves scrollback focused; Space returns to the prompt.
    harness.inject_keys(b" ").expect("focus prompt");
    harness.update(Duration::from_millis(200));

    harness
        .inject_keys(b"/debug scroll\r")
        .expect("submit /debug scroll");
    harness
        .wait_for_text("scroll debug", Duration::from_secs(5))
        .expect("HUD title after /debug scroll");

    // Toggle back off: the next frame repaints the corner without the HUD.
    harness
        .inject_keys(b"/debug scroll\r")
        .expect("submit /debug scroll again");
    harness.update(Duration::from_millis(500));
    assert!(
        !harness.contains_text("scroll debug"),
        "HUD must clear after the second /debug scroll\nscreen:\n{}",
        harness.screen_contents()
    );
    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        running && !harness.contains_text("panicked"),
        "pager broke during the /debug scroll round trip\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
