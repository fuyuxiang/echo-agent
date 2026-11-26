// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── A5: env-forced scroll settings reach the live scroll config ───────────
//
// `ECHO_AGENT_SCROLL_MODE=wheel` + `ECHO_AGENT_SCROLL_LINES=1` on a `TERM_PROGRAM=zed`
// harness (Zed profile: ept=1, wheel_lines=3) must price a 3-event burst at
// exactly 3 rows — one row per event:
//
//   desired = 3 events x (1 line / 1 ept) x speed 1.0 = 3
//
// Each env var is observably load-bearing:
// - Without ECHO_AGENT_SCROLL_LINES=1, Zed's wheel profile prices the burst at
//   3 lines/event → 9 rows.
// - Without ECHO_AGENT_SCROLL_MODE=wheel, Auto finalizes a 3-event ZERO-interval
//   burst as Trackpad on ept=1 (event_count > 2), repricing it at the
//   normalized trackpad divisor (~1 row) — and mid-burst timing jitter picks
//   between the two. Forcing wheel removes that classification variance
//   entirely, which is what makes an EXACT row assertion CI-safe.
//
// Remaining determinism notes: back-to-back PTY writes only compress arrival
// gaps (no mid-burst >80ms split; see the driver contract in `scroll.rs`),
// the per-flush cap floor (6) exceeds the 3-line total, the wheel path has
// no acceleration, and the harness's hermetic ECHO_AGENT_HOME pins scroll_speed at
// its default (50 → 1.0x).

/// 120 one-row markers ≫ the 50-row PTY: early markers sit off-screen-top.
const MARKER_COUNT: usize = 120;

/// One wheel notch worth of reports on an ept=1 brand: 3 distinct events.
const BURST_EVENTS: usize = 3;

/// Exactly 1 row per event under the forced env (see header math).
const EXPECTED_ROWS: usize = 3;

/// **Env-forced wheel pricing e2e.** The `ECHO_AGENT_SCROLL_MODE` /
/// `ECHO_AGENT_SCROLL_LINES` overrides must reach the live config: a 3-event burst
/// scrolls the viewport up by exactly 3 rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn forced_wheel_mode_env_scrolls_exact_rows() {
    let (mut harness, _content, top_before) = spawn_bottom_pinned_marker_scrollback_with_env(
        MARKER_COUNT,
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
        BURST_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        std::time::Duration::ZERO,
    );
    // Outlasts the 80ms stream gap + finalize cadence with CI slack.
    harness.update(std::time::Duration::from_millis(600));

    let running = harness.is_running().expect("poll pager liveness");
    assert!(
        running && !harness.contains_text("panicked"),
        "pager broke during the forced-wheel burst\nscreen:\n{}",
        harness.screen_contents()
    );

    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the burst\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    // Direction guard first: a wrong-direction regression must fail with the
    // suite's screen-dump diagnostics, not a bare usize subtract overflow.
    assert!(
        top_after < top_before,
        "wheel-up burst did not scroll the viewport up: topmost visible marker \
         {} → {} (expected a decrease)\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );
    assert_eq!(
        top_before - top_after,
        EXPECTED_ROWS,
        "forced wheel + 1 line/tick must move exactly {EXPECTED_ROWS} rows \
         ({} → {})\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
