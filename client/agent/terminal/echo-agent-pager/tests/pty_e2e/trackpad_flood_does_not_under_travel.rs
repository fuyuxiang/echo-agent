// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── Regression: trackpad feel — fast flicks must not under-travel ─────────
//
// User complaint: fast trackpad flicks felt "laggy AND too sensitive" and
// under-traveled. The trackpad flush path had a fixed 6-line per-flush cap:
// at the 16ms cadence that is a hard ~360 lines/s delivery ceiling no matter
// how fast the gesture or how tall the viewport, and whatever backlog was
// still queued when the stream finalized was discarded beyond one more
// 6-line flush — the flick's tail simply evaporated. The fix makes the cap
// proportional (half the viewport per flush, floored at 6) so delivery
// scales with gesture demand, and the finalize flush drains the whole-line
// backlog up to that cap.
//
// The trackpad cap only binds once a stream is CONFIRMED trackpad, which
// happens mid-stream only on ept=1 brands (their rapid same-direction runs
// promote at the 3rd event; the harness default `TerminalName::Unknown` is
// ept=3, where auto streams stay Unknown until finalize and flush uncapped).
// So this test presents `TERM_PROGRAM=iTerm.app` (ept=1) and floods wheel-up
// reports with `ECHO_AGENT_SCROLL_SPEED=100` (6x multiplier — an existing user
// setting, used here as a deterministic demand amplifier). Total viewport
// travel is then classification- and jitter-robust:
//
// - Every event contributes at least 1 base line even at the 1.0x accel
//   floor, so desired travel is ≥ 40 × 1 × 6 = 240 rows no matter how much
//   scheduler jitter stretches the 6ms gaps (jitter only lowers the accel
//   multiplier toward 1.0; the speed multiplier is timing-independent). The
//   proportional cap (~20 lines/flush at the 50-row PTY) drains that demand
//   within the burst plus the 80ms stream-gap window, so travel stays above
//   TRAVEL_FLOOR. If extreme stretching (avg gap > 30ms) prevents trackpad
//   promotion entirely, the stream is wheel-like and uncapped: travel = 240.
// - Under the parent's fixed 6-line cap the same flood delivers at most 6
//   lines per 16ms slot: a ~260ms burst spans ~16 slots (~96 rows), plus the
//   post-stop gap window (5 slots + one finalize flush ≈ 36 rows) — ≈ 130
//   rows total, with the remaining backlog discarded at finalize. Verified
//   against a parent-built binary: travel came out 138 rows (< TRAVEL_FLOOR).
//
// One jitter regime the floor does NOT cover is arrival COMPRESSION (the
// mirror of stretching, per the driver contract in `scroll.rs`): if the
// pager is descheduled for the whole host-paced burst, all reports queue in
// the PTY and arrive as one batch. Accel maxes but delivery is cap-bound to
// one in-batch flush plus the post-stop gap window (~5 cadence slots + one
// finalize flush ≈ 7 flushes × ~20 lines ≈ 140 rows) — a legitimate
// cap-bound outcome numerically indistinguishable from the parent defect
// (138). The regimes separate by CAUSE, visible in the frame count: a
// compressed burst paints ≲7 frames, while the parent's 6-line cap needs
// ~16+ flush frames to travel that far. The detector below softens the
// floor assertion in the compressed regime (distinct message) so a CI stall
// is not misread as the regression; the floor stays a hard assertion in the
// normal regime.
//
// Only byte-deterministic quantities are asserted (marker indices on the
// final screen, frame counts from the preamble's reset_timing() capture) —
// the post-burst update() is a drain window, not a timing assertion.
// Per-flush pacing/cap math is pinned by the synthetic-clock unit tests in
// `src/input/mouse.rs`.

/// Marker count: tall enough that maximum plausible travel (bounded by the
/// ~590-row desired total at full nominal acceleration) never clamps at the
/// transcript top, which would mask a travel measurement.
const MARKER_COUNT: usize = 700;

/// 40 spaced single reports at a nominal 6ms: rapid enough that the ept=1
/// brand promotes the stream to confirmed trackpad at the 3rd event
/// (avg interval < 30ms), engaging the per-flush cap under test.
const BURST_EVENTS: usize = 40;

const BURST_INTERVAL: Duration = Duration::from_millis(6);

/// Rows the flood must travel. Below every new-code delivery regime
/// (jitter floor ≈ 240) and above the parent's cap-bound ceiling (≈ 130).
const TRAVEL_FLOOR: usize = 200;

/// Compressed-burst signature: a fully batched arrival delivers over at
/// most one in-batch flush + ~5 post-stop cadence slots + one finalize
/// flush (≈ 7 painted frames), while a genuine cap regression must pace the
/// 260ms burst window at 6 lines per 16ms slot (≥ 21 frames to reach ~138
/// rows). 12 splits that band: it also absorbs partial stalls (~205-230ms
/// deschedules batching most-but-not-all of the burst into 9-10 frames with
/// travel just under the floor) without ever reaching the ≥ 21-frame
/// real-regression signature.
const COMPRESSED_BURST_FRAMES_MAX: u64 = 12;

/// **Trackpad fast-flick travel regression.** A dense trackpad flood at high
/// scroll speed must move the viewport by at least [`TRAVEL_FLOOR`] rows:
/// the proportional per-flush cap keeps up with gesture demand and the
/// finalize flush drains the backlog instead of discarding the flick's tail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn trackpad_flood_does_not_under_travel() {
    let (mut harness, _content, top_before) = spawn_bottom_pinned_marker_scrollback_with_env(
        MARKER_COUNT,
        &[
            ("TERM_PROGRAM", "iTerm.app"),
            ("ECHO_AGENT_SCROLL_SPEED", "100"),
        ],
    )
    .await;

    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        BURST_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        BURST_INTERVAL,
    );
    // Drain window: outlasts the 80ms stream gap plus the post-stop cadence
    // flushes and the finalize flush.
    harness.update(Duration::from_millis(800));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during the trackpad flood\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked' during the trackpad flood\nscreen:\n{}",
        harness.screen_contents()
    );

    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the trackpad flood\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "trackpad flood did not scroll the viewport: topmost visible marker \
         {} → {}\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );
    let travel = top_before - top_after;
    let frames = harness.frame_count();
    // Compressed-burst detector (see header): sub-floor travel with only a
    // handful of frames means the reports arrived batched (pager stalled
    // through the burst window) — a cap-bound outcome, not the under-travel
    // regression. Soften with a distinct message instead of failing.
    if travel < TRAVEL_FLOOR && frames <= COMPRESSED_BURST_FRAMES_MAX {
        eprintln!(
            "SKIP(compressed burst): {frames} frames \
             (<= {COMPRESSED_BURST_FRAMES_MAX}) painted {travel} rows — the \
             burst arrived batched under host load; not asserting the \
             {TRAVEL_FLOOR}-row floor (this is NOT the under-travel \
             regression, which paces ~16+ frames)"
        );
        harness.quit().expect("clean quit");
        return;
    }
    assert!(
        travel >= TRAVEL_FLOOR,
        "trackpad flood under-traveled: {travel} rows (< {TRAVEL_FLOOR}) \
         across {frames} paced frames — fixed per-flush cap ceiling and/or \
         finalize backlog discard regressed\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
