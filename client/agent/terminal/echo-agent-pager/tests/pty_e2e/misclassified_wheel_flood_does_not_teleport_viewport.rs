// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── Regression: wheel-path cap — a misclassified flood must not teleport ──
//
// The parent capped only confirmed-trackpad flushes; wheel and Unknown
// streams flushed their whole backlog in one 16ms slot on the assumption
// that "wheel desired is bounded by physical notches". That assumption
// fails under misclassification (a trackpad the brand table prices as
// ept=3 that never promotes) and terminal-generated momentum bursts: one
// flush teleported the viewport by hundreds of rows. The fix routes every
// stream kind through the proportional cap (max(6, viewport/2) per flush)
// with the excess carried into later cadence slots.
//
// Driver shape: 60 back-to-back wheel-up reports (`Duration::ZERO` — no
// host sleeps) at ECHO_AGENT_SCROLL_SPEED=100 (6x, the existing user setting as
// a demand amplifier). Under the harness terminal (`TerminalName::Unknown`,
// ept=3) the first 3 events land within microseconds, promoting the stream
// to Wheel; desired = 60 x (3/3) x 6.0 = 360 rows. Determinism: the writer
// finishes in microseconds and the reports queue in the PTY buffer, so
// reader-side delays only COMPRESS arrival timing (per the driver contract
// in `scroll.rs`) — a mid-burst >80ms arrival gap (stream split) is
// impossible, and promotion jitter is irrelevant because Unknown prices
// identically on ept=3.
//
// The teleport guard asserts rows-per-painted-frame, which is timing
// independent:
// - New code: every flush (the promotion flush included) passes through
//   the per-flush cap, <= viewport/2 <= 25 rows on the 50-row PTY, so
//   travel/frames <= 25 in every flush pacing/stall regime.
// - Parent: the uncapped backlog lands in the promotion frame plus one or
//   two catch-up flushes (finalize was uncapped too) — ~360 rows over 2-4
//   frames, a ratio >= ~90. Verified against a parent-built binary.
//
// Only byte-deterministic quantities are asserted (marker indices, frame
// count from the preamble's reset_timing() capture); per-flush cap math is
// pinned by the synthetic-clock unit tests in `src/input/mouse.rs`.

/// Marker count: tall enough that even the parent's uncapped 360-row jump
/// cannot clamp at the transcript top (which would mask its teleport).
const MARKER_COUNT: usize = 700;

/// 60 back-to-back reports: 360 rows of demand at 6x speed, several times
/// more than the capped pipeline can deliver inside one stream's lifetime.
const BURST_EVENTS: usize = 60;

/// Teleport signature threshold on rows scrolled per painted frame. Capped
/// delivery is bounded by 25 (see header); the parent's uncapped flood
/// averages >= ~90. 30 splits the bands with margin on both sides.
const MAX_ROWS_PER_FRAME: f64 = 30.0;

/// The flood must visibly scroll: even a fully batched arrival delivers
/// the first-event flush (6), the promotion flush (12) and a capped
/// finalize flush (>= 6); 20 leaves slack for odd viewport rounding.
const TRAVEL_FLOOR: usize = 20;

/// **Misclassified-flood teleport regression.** A dense wheel-classified
/// flood at high scroll speed must deliver its travel in viewport-capped
/// per-frame steps — never as one uncapped multi-hundred-row jump.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn misclassified_wheel_flood_does_not_teleport_viewport() {
    let (mut harness, _content, top_before) = spawn_bottom_pinned_marker_scrollback_with_env(
        MARKER_COUNT,
        &[("ECHO_AGENT_SCROLL_SPEED", "100")],
    )
    .await;

    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        BURST_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    // Drain window: outlasts the 80ms stream gap plus the post-stop cadence
    // flushes and the finalize flush.
    harness.update(Duration::from_millis(800));

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

    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the wheel flood\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "wheel flood did not scroll the viewport: topmost visible marker \
         {} → {}\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );
    let travel = top_before - top_after;
    assert!(
        travel >= TRAVEL_FLOOR,
        "wheel flood barely moved: {travel} rows (< {TRAVEL_FLOOR})\nscreen:\n{}",
        harness.screen_contents()
    );

    let frames = harness.frame_count();
    assert!(
        frames >= 2,
        "a capped flood must paint at least the promotion flush and one \
         follow-up, got {frames} frames"
    );
    let rows_per_frame = travel as f64 / frames as f64;
    assert!(
        rows_per_frame <= MAX_ROWS_PER_FRAME,
        "viewport teleported: {travel} rows over {frames} frames \
         ({rows_per_frame:.1} rows/frame > {MAX_ROWS_PER_FRAME}) — the \
         wheel/Unknown flush path lost its per-flush cap\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
