// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── Infra smoke: timed wheel bursts + viewport markers + frame capture ────
//
// Exercises the scroll-test primitives together against the real pager:
//
// 1. `marker_response` / `marker_screen_row` / `topmost_visible_marker` — a
//    transcript of unique one-row `MARKER-nnnn` lines whose topmost visible
//    index identifies the viewport position, so "the view scrolled up" is a
//    strict index decrease rather than a fragile absolute-row check.
// 2. `send_wheel_burst` / `send_wheel_sequence` — SGR-1006 wheel reports
//    written at a controlled inter-event interval (6ms spaced singles here:
//    a trackpad stream under the harness terminal's ept=3 classification —
//    see the driver doc in `scroll.rs`).
// 3. The harness's live frame capture — `reset_timing()` before the burst,
//    `frame_count()` after the drain (the `renders_on_action.rs` pattern).
//    Only the byte-deterministic frame count is asserted; no wall-clock.
//
// This is an infrastructure smoke, not a regression repro: it pins the
// contracts scroll-pacing tests rely on (off-screen-top markers scroll INTO
// view, at least one repaint happens, repaints never exceed one per wheel
// event). The frame bound is an AMPLIFICATION bound — real cadence
// coalescing (16ms `REDRAW_CADENCE` → ~12 frames for this burst) is left to
// the behavioral tests. All three assertions hold under either wheel or
// trackpad classification, so they are jitter-proof even when host load
// stretches the nominal 6ms gaps.

/// Marker count: 120 one-row lines ≫ the 50-row PTY, so early markers sit
/// off-screen-top once the finished stream pins the view to the bottom.
const MARKER_COUNT: usize = 120;

/// 30 spaced single reports at a nominal 6ms — a trackpad-classified flood
/// under the harness terminal.
const BURST_EVENTS: usize = 30;

const BURST_INTERVAL: Duration = Duration::from_millis(6);

/// **Wheel-burst scroll infra smoke.** A closely spaced wheel-up burst over a
/// marker transcript must scroll previously off-screen-top markers into view,
/// without a panic, producing at least one repaint frame and at most one
/// frame per wheel event (no frame amplification).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wheel_burst_scrolls_viewport_without_frame_amplification() {
    let (mut harness, _content, top_before) =
        spawn_bottom_pinned_marker_scrollback(MARKER_COUNT).await;

    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        BURST_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        BURST_INTERVAL,
    );
    harness.update(Duration::from_millis(600));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during the wheel burst\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked' during the wheel burst\nscreen:\n{}",
        harness.screen_contents()
    );

    // The viewport scrolled: the topmost visible marker index strictly
    // decreased, i.e. a marker that was off-screen-top is now on screen.
    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the wheel burst\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "wheel-up burst did not scroll the viewport: topmost visible marker \
         {} → {} (expected a decrease)\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );

    // Amplification bound on the live frame capture: the burst repainted at
    // least once, and never more than once per wheel event. (Durations are
    // not asserted: the no-drain driver means chunks were parsed at drain
    // time, and wall-clock is load-sensitive anyway.)
    let frames = harness.frame_count();
    assert!(
        frames >= 1,
        "wheel burst produced no repaint frames (no ?2026h/l pairs after reset_timing)"
    );
    assert!(
        frames <= BURST_EVENTS as u64,
        "burst of {BURST_EVENTS} wheel events produced {frames} frames — more than one \
         repaint per event (frame amplification)"
    );

    // Driver shape check: a mixed-direction sequence (momentum reversal) must
    // keep the pager alive — no position assertion, direction handling is for
    // the behavioral tests.
    let reversal = [
        SGR_SCROLL_UP,
        SGR_SCROLL_UP,
        SGR_SCROLL_DOWN,
        SGR_SCROLL_UP,
        SGR_SCROLL_DOWN,
        SGR_SCROLL_DOWN,
    ];
    send_wheel_sequence(
        &mut harness,
        &reversal,
        WHEEL_ROW,
        WHEEL_COL,
        BURST_INTERVAL,
    );
    harness.update(Duration::from_millis(300));
    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        running && !harness.contains_text("panicked"),
        "pager broke on a mixed-direction wheel sequence\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
