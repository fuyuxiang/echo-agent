//! Scroll-test support: timed wheel-burst drivers and viewport-marker
//! helpers. Scroll tests import via `use super::scroll::*;` alongside
//! `use super::common::*;`.
//!
//! Frame capture convention: use the harness's live parser — `reset_timing()`
//! before the interaction, `frame_timings()` / `frame_count()` after the
//! drain (the `renders_on_action.rs` pattern). Frame COUNT and per-frame
//! CHARS are byte-deterministic and CI-assertable; DURATIONS are not — they
//! are load-sensitive, and under the no-drain drivers below the burst's
//! chunks are parsed during the post-burst `update()`, so durations reflect
//! drain pacing rather than real frame timing.

use std::time::Duration;

use echo_agent_pager_pty_harness::PtyHarness;
pub(crate) use echo_agent_pager_pty_harness::{SGR_SCROLL_DOWN, SGR_SCROLL_UP};

use super::common::{
    AgentTurnExpectation, ContentController, DEFAULT_COLS, DEFAULT_ROWS, MOCK_RESPONSE_SENTINEL,
    PROMPT, WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT, locate_screen_text, pager_binary, sgr_mouse,
};

/// Wheel-report position, 0-based (row,col): inside the scrollback pane at
/// the default 50×120 PTY. Encodes to the same bytes as the raw constants in
/// `resize_preserves_scroll_position.rs` (wire `40;12`).
pub(crate) const WHEEL_ROW: u16 = 11;

pub(crate) const WHEEL_COL: u16 = 39;

/// Emit one SGR (DECSET 1006) wheel press report per entry of `btns`
/// ([`SGR_SCROLL_UP`] / [`SGR_SCROLL_DOWN`]; a mixed slice drives direction
/// reversals mid-stream) at 0-based (row,col) via [`sgr_mouse`], sleeping
/// `interval` host-side (`std::thread::sleep`) BETWEEN writes — never before
/// the first or after the last.
///
/// The pager classifies a scroll stream from inter-event ARRIVAL timing
/// (constants in pager `src/input/mouse.rs`), and the harness terminal
/// (`TERM=xterm-256color`, `TERM_PROGRAM` stripped → `TerminalName::Unknown`)
/// resolves to `events_per_tick = 3`. On such ept≥2 terminals a stream
/// promotes to Wheel only when the first 3 reports land within 12ms of the
/// stream's start (`DEFAULT_WHEEL_TICK_DETECT_MAX_MS`); anything slower never
/// promotes and finalizes as Trackpad once a >80ms `STREAM_GAP` closes the
/// stream. So under this harness: `Duration::ZERO` (back-to-back reports)
/// drives wheel clicks, while a long run of spaced reports — even 40ms apart —
/// is a trackpad stream. "Slow interval = wheel" holds only on ept=1 brands
/// (iTerm2 / WezTerm / VS Code embeds, via `trackpad_detect_max_interval_ms`
/// 30–60ms), and 8ms is the acceleration fast band (`ACCEL_INTERVAL_FAST_MS`),
/// not a classifier threshold.
///
/// `interval` only LOWER-BOUNDS each gap: sleep plus scheduler jitter can
/// stretch any gap under CI load (a nominal 6ms past the 12ms promotion
/// window, say). Tests asserting classification-dependent behavior must
/// tolerate or detect stretched gaps; prefer assertions that hold under
/// either classification (scrolled at all, ≥1 frame, ≤1 frame per event).
/// Deliberately dumb: no draining, no assertions — callers `update()`
/// afterwards and assert on screen state / frame captures.
///
/// Harness mirror: the scroll-matrix runner's gesture-replay loop
/// (`echo-agent-pager-pty-harness/src/scroll_matrix/runner.rs`) ports this
/// send loop onto `WheelStep` tables — apply fixes there too.
pub(crate) fn send_wheel_sequence(
    h: &mut PtyHarness,
    btns: &[u16],
    row: u16,
    col: u16,
    interval: Duration,
) {
    for (i, &btn) in btns.iter().enumerate() {
        if i > 0 && !interval.is_zero() {
            std::thread::sleep(interval);
        }
        h.inject_keys(sgr_mouse(btn, row, col, 'M').as_bytes())
            .expect("inject SGR wheel report");
    }
}

/// [`send_wheel_sequence`] for the common case: `count` same-direction reports.
pub(crate) fn send_wheel_burst(
    h: &mut PtyHarness,
    btn: u16,
    count: usize,
    row: u16,
    col: u16,
    interval: Duration,
) {
    let btns = vec![btn; count];
    send_wheel_sequence(h, &btns, row, col, interval);
}

/// Prefix shared by [`marker_line`] and the top-down screen parse in
/// [`topmost_visible_marker`].
const MARKER_PREFIX: &str = "MARKER-";

/// Unique numbered marker line `n` (`MARKER-0042`). Zero-padded so every
/// marker is the same width and no marker is a substring of another within a
/// [`marker_response`] transcript.
pub(crate) fn marker_line(n: usize) -> String {
    format!("{MARKER_PREFIX}{n:04}")
}

/// A response of `count` numbered marker lines inside a fenced code block, so
/// markdown renders exactly one row per marker with no soft-wrap reflow (same
/// trick as `tall_response` in `common.rs`). Feed to `content.set_response(..)`;
/// scroll tests then assert viewport movement via [`marker_screen_row`] /
/// [`topmost_visible_marker`] deltas instead of fragile absolute positions.
pub(crate) fn marker_response(sentinel: &str, count: usize) -> String {
    let mut s = String::with_capacity(count * 16 + 64);
    s.push_str("```\n");
    s.push_str(sentinel);
    s.push('\n');
    for i in 0..count {
        s.push_str(&marker_line(i));
        s.push('\n');
    }
    s.push_str("```\n");
    s
}

/// Current screen row (0-based) of `marker`, or `None` when it is off-screen.
pub(crate) fn marker_screen_row(h: &PtyHarness, marker: &str) -> Option<u16> {
    locate_screen_text(&h.screen_contents(), marker).map(|(row, _)| row)
}

/// Index of the topmost marker on screen: a single top-down pass parsing the
/// first `MARKER-nnnn` occurrence (`None` when no marker is visible; malformed
/// hits are skipped). Scrolling UP strictly decreases it by the number of rows
/// scrolled — the primitive for "viewport moved by K rows / did not jump"
/// assertions.
pub(crate) fn topmost_visible_marker(h: &PtyHarness) -> Option<usize> {
    h.screen_contents().lines().find_map(|line| {
        let digits_at = line.find(MARKER_PREFIX)? + MARKER_PREFIX.len();
        line.get(digits_at..digits_at + 4)?.parse().ok()
    })
}

/// Spawn the pager over a `marker_count`-marker transcript and drive it to
/// the primed scroll-test state (the shared preamble of every wheel pacing
/// test, per the `drive_to_scrollback_with_turn` precedent): response fully
/// streamed, viewport bottom-pinned with the first marker off-screen-top,
/// scrollback focused via Tab (Esc would silently arm the rewind picker,
/// whose ~800ms expiry redraw lands a non-burst frame in the capture
/// window; wheel reports are position-routed regardless of focus), then
/// quiesced and `reset_timing()` applied — counted frames come only from
/// what the caller does next.
///
/// Returns the harness, the content controller (keep it alive: it owns the
/// mock server), and the topmost visible marker index as the movement
/// baseline. Panics with the screen contents on any setup violation.
///
/// Destructure the controller as `_content`, never `_` — a `_` binding drops
/// it immediately, killing the mock server mid-test and surfacing as a
/// confusing 60s stream timeout instead of an obvious failure.
///
/// Harness mirror: `spawn_settled_marker_session` in
/// `echo-agent-pager-pty-harness/src/scroll_matrix/session.rs` is a port of
/// this preamble — fixes must flow both ways.
pub(crate) async fn spawn_bottom_pinned_marker_scrollback(
    marker_count: usize,
) -> (PtyHarness, ContentController, usize) {
    spawn_bottom_pinned_marker_scrollback_with_env(marker_count, &[]).await
}

/// [`spawn_bottom_pinned_marker_scrollback`] with extra pager env vars
/// (e.g. `ECHO_AGENT_SCROLL_SPEED`) appended to the controller's env.
pub(crate) async fn spawn_bottom_pinned_marker_scrollback_with_env(
    marker_count: usize,
    extra_env: &[(&str, &str)],
) -> (PtyHarness, ContentController, usize) {
    let content = ContentController::start().await.expect("start content");
    content.set_response(marker_response(MOCK_RESPONSE_SENTINEL, marker_count));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        extra_env,
    )
    .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    // The LAST marker is the last thing streamed → the whole transcript is in.
    harness
        .wait_for_text(&marker_line(marker_count - 1), Duration::from_secs(60))
        .expect("response finished streaming");
    harness.update(Duration::from_millis(500));

    // Setup guards: bottom-pinned after the stream, the first marker must be
    // off-screen-top (the transcript overflows the viewport) and some later
    // marker visible — otherwise there is nothing to scroll into view.
    assert!(
        marker_screen_row(&harness, &marker_line(0)).is_none(),
        "setup: {} already visible → transcript not taller than the screen\nscreen:\n{}",
        marker_line(0),
        harness.screen_contents()
    );
    let top_before = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "setup: no marker visible after the stream\nscreen:\n{}",
            harness.screen_contents()
        )
    });

    harness.inject_keys(b"\t").expect("focus scrollback (tab)");
    harness.update(Duration::from_millis(500));
    // Footer wait is best-effort, as in drive_to_scrollback_with_turn.
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(5));

    // Quiesce, then live-parser watermark (renders_on_action.rs pattern).
    harness.update(Duration::from_millis(300));
    harness.reset_timing();

    (harness, content, top_before)
}

/// End marker of a [`spawn_streaming_marker_turn`] stream: the final streamed
/// word. The spawn guards pin it off-screen while the tail is in flight;
/// bottom-pinned follow renders it once the viewport tracks the stream to
/// its end — the witness for both "stream still in flight" and "viewport
/// followed the live bottom" assertions.
pub(crate) const STREAM_END_SENTINEL: &str = "STREAMDONE";

/// Turn text for [`spawn_streaming_marker_turn`]: the whole fenced
/// `marker_count`-marker block rides the FIRST delta (the mock splits deltas
/// on single spaces and the block contains none), so scrollable content
/// exists immediately; the space-separated `tail_words` tail then streams
/// word-by-word, ending in [`STREAM_END_SENTINEL`].
fn streaming_marker_turn_text(marker_count: usize, tail_words: usize) -> String {
    let mut s = marker_response(MOCK_RESPONSE_SENTINEL, marker_count);
    for i in 0..tail_words {
        s.push_str(&format!("TAIL-{i:04} "));
    }
    s.push_str(STREAM_END_SENTINEL);
    s
}

/// Spawn the pager onto a turn that is STILL STREAMING and provably cannot
/// complete (the shared preamble of the mid-stream wheel tests): the marker
/// block arrives on the first delta, the tail keeps deltas in flight at
/// `chunk_delay` per SSE event, and a matched expectation prevents terminal
/// completion until released. `extra_env` is appended to the controller's
/// pager env. Returns the harness, content controller, blocked turn, and the
/// bottom-pinned topmost visible marker index; setup asserts that marker zero
/// is above the viewport and [`STREAM_END_SENTINEL`] has not arrived.
///
/// Destructure the controller into a live binding (`content` / `_content`),
/// never `_` — a `_` binding drops it immediately, killing the mock server
/// mid-test and surfacing as a confusing stream timeout.
///
/// Harness mirror: `spawn_streaming_marker_session` in
/// `echo-agent-pager-pty-harness/src/scroll_matrix/session.rs` is a port of
/// this preamble — fixes must flow both ways.
pub(crate) async fn spawn_streaming_marker_turn(
    marker_count: usize,
    tail_words: usize,
    chunk_delay: Duration,
    extra_env: &[(&str, &str)],
) -> (PtyHarness, ContentController, AgentTurnExpectation, usize) {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(chunk_delay));
    let turn = content.expect_agent_turn_blocked(
        "streaming marker turn",
        streaming_marker_turn_text(marker_count, tail_words),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        extra_env,
    )
    .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    // The last marker rides the first delta, so this waits only for the
    // stream to START, not to finish — the tail is still streaming after.
    harness
        .wait_for_text(&marker_line(marker_count - 1), Duration::from_secs(30))
        .expect("marker block streamed");
    harness.update(Duration::from_millis(300));

    // Setup guards, all taken while bottom-pinned in follow mode:
    // the transcript overflows the viewport (something to scroll to)…
    assert!(
        marker_screen_row(&harness, &marker_line(0)).is_none(),
        "setup: {} already visible → transcript not taller than the screen\nscreen:\n{}",
        marker_line(0),
        harness.screen_contents()
    );
    // …and the last chunk has NOT streamed yet (bottom-pinned follow shows
    // the newest content, so a finished stream would render the sentinel).
    assert!(
        !harness.contains_text(STREAM_END_SENTINEL),
        "setup: {STREAM_END_SENTINEL} already on screen — the paced tail finished \
         before the test's wheel activity could overlap the stream\nscreen:\n{}",
        harness.screen_contents()
    );
    let top_before = topmost_visible_marker(&harness).unwrap_or_else(|| {
        panic!(
            "setup: no marker visible mid-stream\nscreen:\n{}",
            harness.screen_contents()
        )
    });

    (harness, content, turn, top_before)
}
