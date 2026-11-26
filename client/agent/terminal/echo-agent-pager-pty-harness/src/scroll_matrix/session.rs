//! Marker-session preambles: spawn the pager into the primed state a matrix
//! cell's gesture assumes, ported from the pager's `tests/pty_e2e/scroll.rs`
//! (`spawn_bottom_pinned_marker_scrollback[_with_env]` /
//! `spawn_streaming_marker_turn`) onto this crate's public API so the A13
//! runner — which lives here, not in the pager's test tree — can reuse the
//! proven construction.
//!
//! ## Controller-drop footgun (kept from the originals)
//!
//! Destructure the returned controller into a LIVE binding (`content` /
//! `_content`), never `_` — a `_` binding drops it immediately, killing the
//! mock server mid-session and surfacing as a confusing 60s stream timeout
//! instead of an obvious failure.
//!
//! ## Streaming sessions
//!
//! [`spawn_streaming_marker_session`] returns a blocked turn expectation. The
//! caller releases it after the gesture and before quitting.

use std::path::Path;
use std::time::Duration;

use crate::PtyHarness;
use crate::content::{AgentTurnExpectation, ContentController};

/// Transcript state a cell's gesture starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// Response fully streamed, viewport bottom-pinned, scrollback focused.
    Settled,
    /// Same preamble as [`SessionKind::Settled`] — the settle leaves the
    /// viewport pinned to the bottom. A separate kind because the cell's
    /// point is the pin itself (down-scroll must clamp: G7 / I-SCREEN),
    /// not just "there is history above".
    BottomPinned,
    /// Turn still streaming (paced deltas + held completion gate) when the
    /// gesture runs — mid-stream by construction (G8).
    Streaming,
}

/// PTY geometry shared by every matrix session (the pager e2e default:
/// large enough that the welcome screen never wraps).
pub const SESSION_ROWS: u16 = 50;
pub const SESSION_COLS: u16 = 120;

/// Wheel-report position, 0-based (row, col): inside the scrollback pane at
/// the [`SESSION_ROWS`]×[`SESSION_COLS`] PTY (wire encodes as SGR `40;12`).
pub const WHEEL_ROW: u16 = 11;
pub const WHEEL_COL: u16 = 39;

/// Welcome-screen sentinel: the menu label `Quit` (case-sensitive, so the
/// lowercase authenticating hint can't match early).
const WELCOME_SCREEN_SENTINEL: &str = "Quit";
const WELCOME_TIMEOUT: Duration = Duration::from_secs(20);
/// Prompt submitted to the agent — short enough never to wrap.
const PROMPT: &str = "go";
/// First line inside the marker code block (the mock response sentinel).
const MOCK_RESPONSE_SENTINEL: &str = "MOCKRESPONSE";

/// Prefix shared by [`marker_line`] and the screen parses.
const MARKER_PREFIX: &str = "MARKER-";

/// End marker of a streaming session's tail: the final streamed word, kept
/// off-screen while the tail is in flight (witness for "still streaming").
pub const STREAM_END_SENTINEL: &str = "STREAMDONE";

/// Unique numbered marker line (`MARKER-0042`), zero-padded so no marker is
/// a substring of another within a [`marker_response`] transcript.
pub fn marker_line(n: usize) -> String {
    format!("{MARKER_PREFIX}{n:04}")
}

/// `count` numbered marker lines inside a fenced code block — markdown
/// renders exactly one row per marker with no soft-wrap reflow, so marker
/// index deltas equal row deltas.
pub fn marker_response(count: usize) -> String {
    let mut s = String::with_capacity(count * 16 + 64);
    s.push_str("```\n");
    s.push_str(MOCK_RESPONSE_SENTINEL);
    s.push('\n');
    for i in 0..count {
        s.push_str(&marker_line(i));
        s.push('\n');
    }
    s.push_str("```\n");
    s
}

/// Current screen row (0-based) of `marker`, or `None` when off-screen.
pub fn marker_screen_row(harness: &PtyHarness, marker: &str) -> Option<u16> {
    harness
        .screen_contents()
        .lines()
        .position(|line| line.contains(marker))
        .map(|row| row as u16)
}

/// Index of the topmost marker on screen (`None` when none visible;
/// malformed hits skipped). Scrolling UP strictly decreases it by the rows
/// scrolled — the "viewport moved by K rows" primitive (I-SCREEN's input).
pub fn topmost_visible_marker(harness: &PtyHarness) -> Option<usize> {
    topmost_marker_in(&harness.screen_contents())
}

fn topmost_marker_in(screen: &str) -> Option<usize> {
    screen.lines().find_map(|line| {
        let digits_at = line.find(MARKER_PREFIX)? + MARKER_PREFIX.len();
        line.get(digits_at..digits_at + 4)?.parse().ok()
    })
}

/// Streaming-session defaults: the marker block rides the FIRST delta; a
/// 240-word tail at 30ms per SSE event then streams for ≥7s — wider than
/// any gesture table (G9b, the longest, spans ~0.4s) plus its finalize
/// wait, so the gesture provably overlaps the stream. Values proven by the
/// pager's `wheel_*_mid_stream` e2e tests.
pub const STREAMING_TAIL_WORDS: usize = 240;
pub const STREAMING_CHUNK_DELAY: Duration = Duration::from_millis(30);

/// Spawn `binary` (the pager under test — the matrix runner plumbs its
/// `--binary` override here; tests pass `env::pager_binary()`) for `kind`
/// over a `marker_count`-marker transcript with `extra_env` appended to the
/// mock's pager env (terminal-class markers, `ECHO_AGENT_SCROLL_*`,
/// `ECHO_AGENT_SCROLL_LOG` — the PTY spawn strips host-terminal identity first,
/// so injected markers always win).
///
/// `blocked_turn` is present only for a streaming session.
pub async fn spawn_marker_session(
    binary: &Path,
    kind: SessionKind,
    marker_count: usize,
    extra_env: &[(&str, &str)],
) -> (
    PtyHarness,
    ContentController,
    usize,
    Option<AgentTurnExpectation>,
) {
    match kind {
        SessionKind::Settled | SessionKind::BottomPinned => {
            let (harness, content, baseline) =
                spawn_settled_marker_session(binary, marker_count, extra_env).await;
            (harness, content, baseline, None)
        }
        SessionKind::Streaming => {
            spawn_streaming_marker_session(
                binary,
                marker_count,
                STREAMING_TAIL_WORDS,
                STREAMING_CHUNK_DELAY,
                extra_env,
            )
            .await
        }
    }
}

/// Shared spawn: mock content up, pager in a [`SESSION_ROWS`]×
/// [`SESSION_COLS`] PTY with content env + `extra_env`, welcome waited.
fn spawn_pager(
    binary: &Path,
    content: &ContentController,
    extra_env: &[(&str, &str)],
) -> PtyHarness {
    let mut harness = PtyHarness::new_in_sandbox(
        binary,
        SESSION_ROWS,
        SESSION_COLS,
        &[],
        content.sandbox(),
        extra_env,
        None,
    )
    .expect("spawn pager with content");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
}

/// Panic (with the screen) unless the transcript overflows the viewport:
/// marker 0 off-screen-top and some later marker visible — otherwise the
/// gesture has nothing to scroll into view. Returns the movement baseline.
fn assert_scrollable_baseline(harness: &PtyHarness, context: &str) -> usize {
    assert!(
        marker_screen_row(harness, &marker_line(0)).is_none(),
        "setup: {} already visible → transcript not taller than the screen ({context})\nscreen:\n{}",
        marker_line(0),
        harness.screen_contents()
    );
    topmost_visible_marker(harness).unwrap_or_else(|| {
        panic!(
            "setup: no marker visible ({context})\nscreen:\n{}",
            harness.screen_contents()
        )
    })
}

/// [`SessionKind::Settled`]/[`SessionKind::BottomPinned`] preamble: stream
/// the whole marker transcript, land bottom-pinned in follow mode, focus
/// the scrollback via Tab (Esc would silently arm the rewind picker, whose
/// ~800ms expiry redraw pollutes frame captures; wheel reports are
/// position-routed regardless of focus), quiesce, reset the frame-timing
/// watermark — counted frames then come only from the caller's gesture.
pub async fn spawn_settled_marker_session(
    binary: &Path,
    marker_count: usize,
    extra_env: &[(&str, &str)],
) -> (PtyHarness, ContentController, usize) {
    let content = ContentController::start().await.expect("start content");
    content.set_response(marker_response(marker_count));
    let mut harness = spawn_pager(binary, &content, extra_env);

    // The LAST marker is the last thing streamed → the whole transcript is in.
    harness
        .wait_for_text(&marker_line(marker_count - 1), Duration::from_secs(60))
        .expect("response finished streaming");
    harness.update(Duration::from_millis(500));

    let baseline = assert_scrollable_baseline(&harness, "settled");

    harness.inject_keys(b"\t").expect("focus scrollback (tab)");
    harness.update(Duration::from_millis(500));
    // Footer wait is best-effort (skin-dependent), as in the original.
    let _ = harness.wait_for_text("Space:prompt", Duration::from_secs(5));

    harness.update(Duration::from_millis(300));
    harness.reset_timing();

    (harness, content, baseline)
}

/// [`SessionKind::Streaming`] preamble: the whole fenced marker block rides
/// the first delta (the mock splits deltas on single spaces and the block
/// contains none), the space-separated tail streams word-by-word at
/// `chunk_delay`, and the matched expectation prevents terminal completion
/// until the caller releases it. Setup guards: transcript overflows the
/// viewport and [`STREAM_END_SENTINEL`] is not on screen.
pub async fn spawn_streaming_marker_session(
    binary: &Path,
    marker_count: usize,
    tail_words: usize,
    chunk_delay: Duration,
    extra_env: &[(&str, &str)],
) -> (
    PtyHarness,
    ContentController,
    usize,
    Option<AgentTurnExpectation>,
) {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(chunk_delay));
    let mut turn = marker_response(marker_count);
    for i in 0..tail_words {
        turn.push_str(&format!("TAIL-{i:04} "));
    }
    turn.push_str(STREAM_END_SENTINEL);
    let turn = content.expect_agent_turn_blocked("streaming marker turn", turn);

    let mut harness = spawn_pager(binary, &content, extra_env);
    // The last marker rides the first delta, so this waits only for the
    // stream to START; the tail is still in flight afterwards.
    harness
        .wait_for_text(&marker_line(marker_count - 1), Duration::from_secs(30))
        .expect("marker block streamed");
    harness.update(Duration::from_millis(300));

    assert!(
        !harness.contains_text(STREAM_END_SENTINEL),
        "setup: {STREAM_END_SENTINEL} already on screen — the paced tail finished \
         before the gesture could overlap the stream\nscreen:\n{}",
        harness.screen_contents()
    );
    let baseline = assert_scrollable_baseline(&harness, "mid-stream");

    (harness, content, baseline, Some(turn))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_transcript_shape() {
        assert_eq!(marker_line(42), "MARKER-0042");
        let response = marker_response(3);
        assert!(response.starts_with("```\nMOCKRESPONSE\n"));
        assert!(response.ends_with("MARKER-0002\n```\n"));
        assert_eq!(response.matches(MARKER_PREFIX).count(), 3);
    }

    #[test]
    fn topmost_marker_parses_top_down_and_skips_malformed_hits() {
        let screen = "noise\nMARKER-12 truncated\n x MARKER-0007 y\nMARKER-0003\n";
        assert_eq!(topmost_marker_in(screen), Some(7));
        assert_eq!(topmost_marker_in("no markers here"), None);
    }

    /// The streaming defaults must dwarf the longest gesture: every step
    /// table finishes (delays summed) well inside the paced tail's window,
    /// or the "still streaming" guard could race the gesture.
    #[test]
    fn streaming_window_covers_every_gesture_table() {
        use super::super::gestures::{GestureId, STREAM_GAP_MS};
        let tail_ms = STREAMING_TAIL_WORDS as u64 * STREAMING_CHUNK_DELAY.as_millis() as u64;
        for gesture in GestureId::ALL {
            for ept in [1u16, 3] {
                let span: u64 = gesture.steps(ept).iter().map(|s| s.pre_delay_ms).sum();
                // Gesture + every stream's finalize gap, ×4 headroom.
                let with_finalize = (span + gesture.expected_streams() as u64 * STREAM_GAP_MS) * 4;
                assert!(
                    with_finalize < tail_ms,
                    "{gesture:?} (ept={ept}) span {with_finalize}ms crowds the \
                     {tail_ms}ms streaming window"
                );
            }
        }
    }
}
