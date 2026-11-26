// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── A7: wheel overscroll at the bottom re-engages follow mode ─────────────
//
// User complaint: after wheeling up to read during a streaming turn, wheeling
// back down to the bottom left a "dead wheel tick" before follow mode
// resumed. `scroll_down` now re-engages follow on the first FULLY-CLAMPED
// wheel-down at the bottom (zero rows moved), while a scroll that merely
// lands there (real rows moved) still never engages.
//
// This test drives the whole loop end-to-end mid-stream: wheel up (follow
// exits, viewport parks while deltas keep arriving), wheel back down past
// the bottom (the overscroll), then assert the viewport FOLLOWS the still
// streaming tail — the final chunk becomes visible with no further input.
// With follow left disengaged, new rows accumulate below the parked
// viewport and the sentinel never enters the screen.
//
// Honest scope note: the down burst deliberately over-travels, so it
// contains many clamped events — a PTY cannot pin "exactly one clamped tick
// engages" (any wheel journey back to the bottom lands before it clamps, and
// the pre-fix flag-arming path also engaged within such a burst). The
// single-event semantics are pinned by the `scrollback::state::nav` unit
// tests; this test pins the user-visible contract on the real binary.
// Non-vacuous by negative control: with the engage branch compiled out,
// this test fails (STREAMDONE never becomes visible).
//
// Determinism: `TERM_PROGRAM=zed` + `ECHO_AGENT_SCROLL_MODE=wheel` +
// `ECHO_AGENT_SCROLL_LINES=1` price every SGR report at exactly one row with no
// classification variance (see `forced_wheel_mode_env_scrolls_exact_rows`),
// and the completion gate holds the turn's terminal SSE event so every
// assertion below is provably mid-turn.

/// 240 one-row markers ≫ the 50-row PTY: the up-burst can never clamp at the
/// transcript top, and markers stay visible above the streamed tail.
const MARKER_COUNT: usize = 240;

/// Rows wheeled up to exit follow (1 row per event under the forced env).
const UP_EVENTS: usize = 8;

/// Down-burst size: covers the 8 rows back plus tail growth during the
/// dance (~30ms/word ≈ 2-3 rows/s) with a wide margin; the excess events
/// arrive fully clamped at the bottom — the overscroll under test.
const DOWN_EVENTS: usize = 60;

/// Space-separated tail words streamed one-per-delta after the marker block:
/// 240 words at the 30ms chunk delay ≈ 7s of paced streaming, so the tail is
/// still arriving well after the wheel dance ends.
const TAIL_WORDS: usize = 240;

/// Per-SSE-event pacing so deltas keep arriving while the dance runs.
const CHUNK_DELAY: Duration = Duration::from_millis(30);

/// **Overscroll → follow regression.** Mid-stream: wheel up to exit follow,
/// wheel back down past the bottom, and the viewport must resume following —
/// the stream's final chunk appears with no further input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wheel_overscroll_at_bottom_reengages_follow_mid_stream() {
    // Gated, paced, provably mid-turn transcript with setup guards taken,
    // spawned under the forced-wheel pricing env (see the header).
    let (mut harness, _content, mut turn, top_start) = spawn_streaming_marker_turn(
        MARKER_COUNT,
        TAIL_WORDS,
        CHUNK_DELAY,
        &[
            ("TERM_PROGRAM", "zed"),
            ("ECHO_AGENT_SCROLL_MODE", "wheel"),
            ("ECHO_AGENT_SCROLL_LINES", "1"),
        ],
    )
    .await;

    // Wheel up: exit follow and park the viewport above the live bottom.
    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        UP_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    harness.update(Duration::from_millis(600));
    let top_parked = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the up-burst\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_parked < top_start,
        "wheel-up did not exit follow / move the viewport: topmost visible \
         marker {} → {}\nscreen:\n{}",
        marker_line(top_start),
        marker_line(top_parked),
        harness.screen_contents()
    );

    // Parked witness: deltas keep arriving but the viewport must not move —
    // follow really is off, so the re-engage below is a genuine transition.
    harness.update(Duration::from_millis(500));
    assert_eq!(
        topmost_visible_marker(&harness),
        Some(top_parked),
        "viewport drifted while parked out of follow mode\nscreen:\n{}",
        harness.screen_contents()
    );

    // Wheel down past the bottom: land, then overscroll → follow re-engages.
    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_DOWN,
        DOWN_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    harness.update(Duration::from_millis(800));
    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        running && !harness.contains_text("panicked"),
        "pager broke during the wheel dance\nscreen:\n{}",
        harness.screen_contents()
    );
    // The completion gate is still held, so the turn is provably running.
    assert!(
        harness.contains_text("Responding"),
        "turn ended despite the held completion gate\nscreen:\n{}",
        harness.screen_contents()
    );

    // The core assertion: with follow re-engaged, the still-streaming tail
    // pushes the final chunk into view with NO further input. If the
    // overscroll had not re-engaged follow, new rows would pile up below
    // the viewport and the sentinel would never appear.
    harness
        .wait_for_text(STREAM_END_SENTINEL, Duration::from_secs(40))
        .unwrap_or_else(|_| {
            panic!(
                "{STREAM_END_SENTINEL} never became visible — overscroll did not \
                 re-engage follow mode\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // Release the gate and let the turn complete: the dance didn't wedge it.
    turn.release();
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    while harness.contains_text("Responding") {
        assert!(
            std::time::Instant::now() < deadline,
            "turn never completed after releasing its expectation\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(200));
    }
    tokio::time::timeout(Duration::from_secs(10), turn.wait_satisfied())
        .await
        .expect("streaming marker expectation satisfied");

    harness.quit().expect("clean quit");
}
