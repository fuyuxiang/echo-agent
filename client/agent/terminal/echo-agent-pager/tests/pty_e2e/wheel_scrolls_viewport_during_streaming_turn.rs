// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;
#[allow(unused_imports)]
use super::scroll::*;

// ── Regression: streaming must not starve wheel input ─────────────────────
//
// User complaint: "can't scroll while it's streaming". The event loop's
// `select!` is `biased` with the ACP arm above the input arm, and the ACP
// arm drained every immediately-ready message before re-entering the
// select — so during a token flood wheel/key events sat in `input_rx`
// until `acp_rx` momentarily emptied. The fix gates the ACP arm on
// `input_rx.is_empty()` and bounds its inner drain, so buffered input is
// serviced within one bounded ACP batch.
//
// This test drives a turn that is still visibly streaming (paced deltas +
// a held completion gate) and wheels up mid-stream, asserting the viewport
// moved BEFORE the turn completed. Determinism does not lean on wall-clock:
// the mock's completion gate holds the turn's terminal SSE event until the
// test releases it, so "the stream had not completed when movement was
// observed" is true by construction; the on-screen witnesses (status label,
// last-chunk sentinel) are content-ordering assertions on top of that.
//
// Honest scope note: a PTY test cannot force `acp_rx` to be continuously
// non-empty at the exact instant wheel reports land (that interleaving is
// scheduler-owned; paced chunks let even the old code service input between
// deltas), so this is the strongest deterministic approximation, not a
// discriminator of the starvation mechanism itself: it pins the user-visible
// contract — wheel input moves the viewport while ACP traffic is in flight
// and the turn is provably unfinished — deterministically, on any load.

/// 240 one-row markers ≫ the 50-row PTY: the up-burst can never clamp at the
/// transcript top (30 events × ≤3 lines/event ≈ 90 lines vs ~190 rows of
/// headroom above the bottom-pinned viewport).
const MARKER_COUNT: usize = 240;

/// 30 spaced single reports at a nominal 6ms — a trackpad-classified flood
/// under the harness terminal's ept=3 profile (see `scroll.rs`).
const BURST_EVENTS: usize = 30;

const BURST_INTERVAL: Duration = Duration::from_millis(6);

/// Space-separated tail words streamed one-per-delta after the marker block,
/// keeping ACP traffic in flight while the test scrolls. 160 words at the
/// 30ms chunk delay ≈ 4.8s of paced streaming — a wide window for the
/// pre-burst "still streaming" guard, small enough to keep the test quick.
const TAIL_WORDS: usize = 160;

/// Per-SSE-event pacing so deltas keep arriving while the wheel burst runs.
const CHUNK_DELAY: Duration = Duration::from_millis(30);

/// **Input-fairness regression.** While a long response is still streaming
/// (deltas paced, completion gated), a wheel-up burst must scroll the
/// viewport — the topmost visible marker index strictly decreases — before
/// the turn completes, and the turn must then complete cleanly on release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wheel_scrolls_viewport_during_streaming_turn() {
    // Gated, paced, provably mid-turn transcript with setup guards taken —
    // see the helper for the construction and the baseline's meaning.
    let (mut harness, _content, mut turn, top_before) =
        spawn_streaming_marker_turn(MARKER_COUNT, TAIL_WORDS, CHUNK_DELAY, &[]).await;

    send_wheel_burst(
        &mut harness,
        SGR_SCROLL_UP,
        BURST_EVENTS,
        WHEEL_ROW,
        WHEEL_COL,
        BURST_INTERVAL,
    );
    // Outlasts the 80ms stream gap so residual/finalize flushes land too.
    harness.update(Duration::from_millis(600));

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during the mid-stream wheel burst\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager rendered 'panicked'\nscreen:\n{}",
        harness.screen_contents()
    );

    // The core assertion: the viewport moved while the turn was still streaming.
    // Strict decrease discriminates cleanly even against concurrent growth:
    // starved input would leave follow mode pinned and the topmost index
    // could only stay or INCREASE as tail rows arrive.
    let top_after = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "no marker visible after the mid-stream burst\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    assert!(
        top_after < top_before,
        "wheel-up during streaming did not move the viewport: topmost visible \
         marker {} → {} (expected a decrease)\nscreen:\n{}",
        marker_line(top_before),
        marker_line(top_after),
        harness.screen_contents()
    );
    // Content-ordering witnesses that this happened mid-turn: the streaming
    // status label is still up and the last chunk is not on screen (the
    // completion gate guarantees the turn itself cannot have ended).
    assert!(
        harness.contains_text("Responding"),
        "movement observed but the streaming status label is gone — the turn \
         ended despite the held completion gate\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text(STREAM_END_SENTINEL),
        "movement observed but {STREAM_END_SENTINEL} is on screen\nscreen:\n{}",
        harness.screen_contents()
    );

    // Release the gate and let the tail finish: the turn must complete
    // (status label clears) — scrolling mid-stream didn't wedge it.
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
