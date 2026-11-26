// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── Regression: scroll pacing — one 16ms scroll clock, no ghost frames ────
//
// User complaint: "scroll is both laggy and too sensitive". Two coupled
// defects in the scroll pipeline:
//
// 1. Cadence-suppressed wheel events returned `InputOutcome::Changed` from
//    `AppView::handle_input`, so the event loop ran a full draw for a frame
//    in which NOTHING moved (a ghost frame). The suppressed motion then
//    landed later as a multi-line jump — per-event render work with none of
//    the per-event movement.
// 2. Residual/finalize flushes were driven by the animation tick (default
//    30fps → ~33ms), not the 16ms `REDRAW_CADENCE` the mouse state machine
//    paces itself by, so between-event flushes rode the wrong clock and
//    arrived up to a slot late.
//
// The fix arms a dedicated scroll clock in the event-loop select
// (`MouseScrollState::scroll_clock_deadline`: 16ms cadence flushes, 80ms
// stream-gap finalize) and makes suppressed events return `Unchanged`, so a
// draw happens exactly when lines were dispatched.
//
// This test floods trackpad-like wheel-up reports at the scrollback pane and
// asserts on the harness's live frame capture:
//   (a) the viewport moved — topmost visible marker index strictly decreased;
//   (b) no amplification — frame count ≤ event count (holds under either
//       wheel or trackpad classification and under jitter-stretched gaps);
//   (c) no ghost frames — every captured frame paints at least
//       `MOVEMENT_CHARS_FLOOR` printable chars. Any ≥1-line viewport shift
//       rewrites the unique digit cells of every visible `MARKER-nnnn` row
//       (dozens of chars), while a no-movement frame diff is EMPTY (the
//       render path discards zero-diff frames entirely, so a counted 2026
//       pair with ~0 chars can only be a reintroduced ghost draw).
//
// Only byte-deterministic quantities are asserted (frame COUNT, per-frame
// CHARS, marker indices) — never durations, which are load-sensitive under
// the no-drain driver (see `scroll.rs`). Wall-clock pacing of the 16ms slots
// is pinned by the synthetic-clock unit tests in `src/input/mouse.rs`.

/// Marker count: 240 one-row lines ≫ the 50-row PTY. Large enough that the
/// burst can never clamp at the transcript top (30 events × ≤3 lines/event
/// at max trackpad acceleration ≈ 90 lines, well short of ~200 rows above
/// the bottom-pinned viewport) — a clamped flush would legitimately paint
/// nothing and break the ghost-frame floor.
const MARKER_COUNT: usize = 240;

/// 30 spaced single reports at a nominal 6ms — a trackpad-classified flood
/// under the harness terminal's ept=3 profile (see `scroll.rs`).
const BURST_EVENTS: usize = 30;

const BURST_INTERVAL: Duration = Duration::from_millis(6);

/// Minimum printable chars for a frame that scrolled the marker viewport.
/// A 1-line shift rewrites ≥1 digit cell in each of the ~40 visible marker
/// rows; a ghost frame carries ~0. 10 sits far from both.
const MOVEMENT_CHARS_FLOOR: usize = 10;

/// **Wheel-flood pacing regression.** A trackpad-like flood over a marker
/// transcript must scroll the viewport with every captured frame painting
/// real movement: at least two coalesced frames (the burst spans many 16ms
/// cadence slots), at most one frame per event, and no frame below the
/// movement char floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wheel_flood_paints_no_ghost_frames() {
    // Capture window spans the burst plus the post-burst residual/finalize
    // flushes (the update() below outlasts the 80ms stream gap).
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
        "pager exited during the wheel flood\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked' during the wheel flood\nscreen:\n{}",
        harness.screen_contents()
    );

    // (a) The viewport scrolled: the topmost visible marker index strictly
    // decreased, i.e. a marker that was off-screen-top is now on screen.
    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the wheel flood\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "wheel-up flood did not scroll the viewport: topmost visible marker \
         {} → {} (expected a decrease)\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );

    // (b) Coalescing bounds. Lower bound 2 is jitter-safe: 30 events at a
    // ≥6ms lower-bounded interval span ≥174ms — many 16ms cadence slots and
    // more than one flush even if stretched gaps split the stream. Upper
    // bound is the amplification cap: never more than one frame per event.
    let frames = harness.frame_count();
    assert!(
        frames >= 2,
        "a flood spanning many cadence slots must repaint more than once, got {frames}"
    );
    assert!(
        frames <= BURST_EVENTS as u64,
        "flood of {BURST_EVENTS} wheel events produced {frames} frames — more than \
         one repaint per event (frame amplification)"
    );

    // (c) No ghost frames: every frame in the burst window painted movement.
    for (i, timing) in harness.frame_timings().iter().enumerate() {
        assert!(
            timing.chars >= MOVEMENT_CHARS_FLOOR,
            "frame {i} of {frames} painted only {} chars (< {MOVEMENT_CHARS_FLOOR}) — \
             a ghost frame that moved no content\nscreen:\n{}",
            timing.chars,
            harness.screen_contents()
        );
    }

    harness.quit().expect("clean quit");
}
