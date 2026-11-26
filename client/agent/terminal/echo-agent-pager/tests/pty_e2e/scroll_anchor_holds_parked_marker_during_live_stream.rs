// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// Diagnostic: after a forced 1-line wheel-up parks a visible marker mid-stream,
// the parked marker must stay put with no further input while the turn is still
// live. The up-burst proves input was delivered; the no-input observation
// isolates drift from extra wheel/key delivery.
//
// Scope limit: the paced tail appends *below* the parked markers in the same
// agent entry. This does not reproduce upstream height growth/removal (unit
// tests cover that). It only pins below-tail + no-input stability.

/// 240 one-row markers ≫ the 50-row PTY: the up-burst cannot clamp at the top.
const MARKER_COUNT: usize = 240;

/// Rows wheeled up to exit follow (1 row per event under the forced env).
const UP_EVENTS: usize = 12;

/// Space-separated tail words after the marker block keep ACP traffic in flight.
const TAIL_WORDS: usize = 200;

/// Per-SSE-event pacing so deltas keep arriving after the wheel-up parks.
const CHUNK_DELAY: Duration = Duration::from_millis(30);

/// **No-input stability after wheel-up.** Mid-stream: exact wheel pricing,
/// wheel up, then observe with no further input. The parked marker's screen
/// row must not move while the turn is still live (tail continues below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn parked_marker_does_not_jolt_during_live_stream_after_wheel_up() {
    let (mut harness, _content, turn, top_start) = spawn_streaming_marker_turn(
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

    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        UP_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        Duration::ZERO,
    );
    harness.update(Duration::from_millis(600));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during the mid-stream wheel-up\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked'\nscreen:\n{}",
        harness.screen_contents()
    );

    let parked_idx = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after wheel-up\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        parked_idx < top_start,
        "wheel-up did not deliver input / move the viewport: topmost visible \
         marker {} → {}\nscreen:\n{}",
        marker_line(top_start),
        marker_line(parked_idx),
        harness.screen_contents()
    );
    let parked_label = marker_line(parked_idx);
    let parked_row = marker_screen_row(&harness, &parked_label).unwrap_or_else(|| {
        panic!(
            "parked marker {parked_label} missing after wheel-up\nscreen:\n{}",
            harness.screen_contents()
        )
    });

    assert!(
        harness.contains_text("Responding"),
        "stream ended before the no-input observation\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text(STREAM_END_SENTINEL),
        "{STREAM_END_SENTINEL} on screen before the no-input observation\nscreen:\n{}",
        harness.screen_contents()
    );

    // No further input: below-tail streaming must not move the parked marker.
    harness.update(Duration::from_millis(800));

    assert!(
        harness.contains_text("Responding"),
        "stream ended during the no-input observation — cannot isolate layout drift\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text(STREAM_END_SENTINEL),
        "{STREAM_END_SENTINEL} appeared during the no-input observation\nscreen:\n{}",
        harness.screen_contents()
    );

    let later_idx = topmost_visible_marker(&harness);
    let later_row = marker_screen_row(&harness, &parked_label);
    assert_eq!(
        later_idx,
        Some(parked_idx),
        "parked marker jolted during live stream with no input: topmost \
         {} → {:?} (row was {parked_row})\nscreen:\n{}",
        marker_line(parked_idx),
        later_idx.map(marker_line),
        harness.screen_contents()
    );
    let row_delta = later_row.map(|r| r as i32 - parked_row as i32);
    assert_eq!(
        later_row,
        Some(parked_row),
        "parked marker {parked_label} moved screen row {parked_row} → {:?} \
         during live stream with no input (delta {row_delta:?})\nscreen:\n{}",
        later_row,
        harness.screen_contents()
    );

    turn.release();
    let deadline = Instant::now() + Duration::from_secs(40);
    while harness.contains_text("Responding") {
        assert!(
            Instant::now() < deadline,
            "turn never completed after release\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(200));
    }

    harness.quit().expect("clean quit");
}
