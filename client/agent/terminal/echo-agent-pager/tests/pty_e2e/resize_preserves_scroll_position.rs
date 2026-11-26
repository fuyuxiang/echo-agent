// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

// ── Reproduction: horizontal resize must not lose the scroll position ─────
//
// Bug: when the user has scrolled UP into echoagent's scrollback (NOT pinned to the
// bottom / not following) and the terminal is resized HORIZONTALLY, echoagent loses
// its scroll position and the view jumps. Root cause:
// `ScrollbackState.scroll_offset` is an ABSOLUTE count of WRAPPED DISPLAY ROWS
// (`scrollback/state/mod.rs`). On a width change `prepare_layout` re-wraps all
// content at the new width and rebuilds the row map, but LEAVES `scroll_offset`
// unchanged (Case 1 full rebuild) — so the same row number now points at
// different content and the viewport jumps. It only happens when NOT following;
// in follow/tail mode echoagent re-pins to the bottom (no jump).
//
// This test parks the marker in the MIDDLE of the viewport while scrolled into
// the middle of the transcript (TOP and BOTTOM sentinels both scrolled off),
// then resizes the WIDTH only. With the bug present the marker JUMPS several
// rows (re-wrapping the content above it changes its display height, but the
// stale `scroll_offset` keeps pointing at the old row number); the fix
// (`ScrollbackState`'s logical-line scroll anchor, captured before the width
// rebuild and restored after) re-pins the viewport-top content across the
// reflow so the marker stays put.
//
// Runs FULLSCREEN (alt-screen): echoagent's mode-independent scroll-anchor logic is
// what's under test. `Viewport::Fullscreen` is autoresized on resize with no DSR
// cursor probe, so echoagent itself re-wraps at the new width, and the alt-screen grid
// is not reflowed by the terminal — so the marker's position reflects echoagent's own
// re-layout, with no harness-reflow confound.

/// Unique marker on its own (non-wrapping) line, with WRAPPING content above it.
const MARKER: &str = "SCROLL_ANCHOR_MARKER_ZZZ";

/// First line of the response. When this is NOT on screen we have scrolled past
/// the top of the response → `scroll_offset > 0` (the regime where the bug
/// lives; at the absolute top staying-at-top is correct, so there is no bug).
const TOP_SENTINEL: &str = "TOP_OF_RESPONSE_AAA";

/// Last line of the response. When this is NOT on screen we are not pinned to
/// the bottom / not following.
const BOTTOM_SENTINEL: &str = "BOTTOM_OF_RESPONSE_QQQ";

/// Number of long, WRAPPING paragraphs placed above the marker. Each wraps to 2
/// display rows at the spawn width (120 cols) and 3 rows at the resize width
/// (80 cols), so the content above the marker gains ~`WRAP_LINES_ABOVE` rows
/// when the terminal narrows. That gain is the jump the stale `scroll_offset`
/// produces.
const WRAP_LINES_ABOVE: usize = 10;

/// Short, NON-wrapping paragraphs placed immediately above the marker. The
/// marker is parked in the middle of the viewport, so there must be enough of
/// these to fill the rows directly above it (the re-wrapping paragraphs stay
/// scrolled off above the viewport) — that way an anchoring fix keeps the marker
/// stable while the stale-offset bug still jumps it.
const GUARD_LINES: usize = 16;

/// Short, non-wrapping filler below the marker. Enough to exceed a viewport so
/// the marker can be parked mid-viewport with the bottom scrolled off.
const FILLER_BELOW: usize = 30;

/// Spawn width (cols). 120 is the harness default.
const WIDE_COLS: u16 = DEFAULT_COLS;

/// Resize width (cols). Narrower → the wrapping lines above the marker grow
/// from 2 to 3 display rows each.
const NARROW_COLS: u16 = 80;

/// Allowed marker drift (viewport rows) across a width-only resize. A correct
/// (scroll-anchored) reflow keeps the marker within a row or two; the bug moves
/// it by ~`WRAP_LINES_ABOVE` rows.
const POS_TOLERANCE: i32 = 2;

/// Build the scripted response: a top sentinel, a block of long WRAPPING
/// paragraphs, a block of short non-wrapping guard paragraphs, the marker on its
/// own line, a long run of short filler, then a bottom sentinel.
///
/// echoagent renders markdown with SOFT line breaks (a single `\n` becomes a space),
/// so every logical line is emitted as its own paragraph (blank line between)
/// to get a predictable one-hard-line-per-line layout where only the long
/// paragraphs re-wrap on a width change.
fn scroll_anchor_response() -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    paragraphs.push(TOP_SENTINEL.to_string());

    // ~220-char unbreakable paragraphs: wrap to 2 display rows at 120 cols and
    // 3 at 80 cols (the height gain a stale scroll_offset turns into a jump).
    // No spaces — the mock emits one SSE event per space-separated token, and
    // `"wrap ".repeat(44)` × 10 paragraphs (~440 events) routinely exceeds the
    // setup wait under 4-way PTY parallelism (visible as TOP_OF_RESPONSE_AAA
    // still on screen when BOTTOM_OF_RESPONSE_QQQ never arrives).
    let wrapping = "W".repeat(220);
    for _ in 0..WRAP_LINES_ABOVE {
        paragraphs.push(wrapping.clone());
    }

    // Short guard paragraphs directly above the marker (do NOT re-wrap on resize).
    for i in 0..GUARD_LINES {
        paragraphs.push(format!("guard-line-{i:02}"));
    }

    paragraphs.push(MARKER.to_string());

    for i in 0..FILLER_BELOW {
        paragraphs.push(format!("below-line-{i:02}"));
    }
    paragraphs.push(BOTTOM_SENTINEL.to_string());

    paragraphs.join("\n\n")
}

/// SGR mouse wheel-up at a position inside the scrollback pane (1-based wire
/// coords). Same bytes as `sgr_mouse(SGR_SCROLL_UP, WHEEL_ROW, WHEEL_COL, 'M')`
/// from `super::scroll` — new scroll tests should use that helper spelling.
const WHEEL_UP: &[u8] = b"\x1b[<64;40;12M";
/// SGR mouse wheel-down (button 65) at the same position.
const WHEEL_DOWN: &[u8] = b"\x1b[<65;40;12M";

/// Park the marker mid-viewport, scrolled into the MIDDLE of the transcript:
/// marker visible, TOP and BOTTOM sentinels both scrolled off. Returns the
/// marker `(row, col)` once parked, else `None`.
///
/// Keyboard scroll keys are captured by the focused input box, so we drive the
/// scroll with the mouse wheel (position-routed to the scrollback pane). The
/// marker enters the viewport from the TOP when scrolling up and descends as we
/// scroll further; a coarse-then-nudge loop settles it into the band. wheel-DOWN
/// nudges raise it back up if a burst overshoots (scroll-down increases
/// `scroll_offset`, moving the marker toward the top).
fn park_marker_mid(h: &mut PtyHarness) -> Option<(u16, u16)> {
    // Band (screen rows) we want the marker parked in. Aim for the MIDDLE of the
    // viewport so the resize-induced jump (~WRAP_LINES_ABOVE rows, observed to
    // move the marker UP toward the top) keeps it on screen rather than scrolling
    // it off either edge.
    const BAND_LO: u16 = 20;
    const BAND_HI: u16 = 32;

    let deadline = Instant::now() + Duration::from_secs(40);

    // Phase 1: coarse scroll up until the marker is on screen at all.
    while Instant::now() < deadline {
        if locate_screen_text(&h.screen_contents(), MARKER).is_some() {
            break;
        }
        for _ in 0..8 {
            let _ = h.inject_keys(WHEEL_UP);
        }
        h.update(Duration::from_millis(200));
    }

    // Phase 2: refine into the mid-viewport band with top & bottom scrolled off.
    while Instant::now() < deadline {
        let screen = h.screen_contents();
        let pos = locate_screen_text(&screen, MARKER);
        let top_vis = screen.contains(TOP_SENTINEL);
        let bottom_vis = screen.contains(BOTTOM_SENTINEL);

        match pos {
            Some((row, _)) if !top_vis && !bottom_vis && (BAND_LO..=BAND_HI).contains(&row) => {
                return pos;
            }
            Some((row, _)) if row > BAND_HI || bottom_vis => {
                // Marker too low (or bottom in view): scroll DOWN to raise it.
                for _ in 0..2 {
                    let _ = h.inject_keys(WHEEL_DOWN);
                }
                h.update(Duration::from_millis(160));
            }
            _ => {
                // Marker too high / top in view / not visible: scroll UP.
                for _ in 0..2 {
                    let _ = h.inject_keys(WHEEL_UP);
                }
                h.update(Duration::from_millis(160));
            }
        }
    }
    locate_screen_text(&h.screen_contents(), MARKER)
}

/// **Resize preserves scroll position (reproduction).**
/// Scroll up into the middle of a long, wrapping transcript so the marker sits
/// mid-viewport (not following, not at the absolute top), then resize the WIDTH
/// only. The marker must stay at ~the same viewport row across the reflow.
/// Without the scroll-anchor fix it jumps (stale `scroll_offset`), which is the bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn resize_preserves_scroll_position() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(scroll_anchor_response());

    // Spawn FULLSCREEN (alt-screen). `Viewport::Fullscreen` is autoresized on a
    // terminal resize with NO DSR cursor probe, so echoagent re-wraps the scrollback
    // at the new width and the stale `scroll_offset` surfaces the jump in echoagent's
    // OWN rendered output. The alt-screen grid is not reflowed by the terminal on
    // resize, so this isolates echoagent's scroll-anchor logic with no harness confound.
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, WIDE_COLS, &content, &[])
            .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Submit a prompt and wait for the whole response to render (bottom sentinel
    // is the last thing streamed → we are following / pinned to the bottom).
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(BOTTOM_SENTINEL, Duration::from_secs(120))
        .unwrap_or_else(|_| {
            panic!(
                "setup: response never finished streaming (no {BOTTOM_SENTINEL:?})\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    harness.update(Duration::from_millis(400));

    // Focus scrollback (Esc) so wheel scroll is unambiguous, then park the
    // marker mid-viewport, scrolled into the middle of the transcript.
    harness.inject_keys(keys::ESC).expect("focus scrollback");
    harness.update(Duration::from_millis(250));

    let before = park_marker_mid(&mut harness).unwrap_or_else(|| {
        panic!(
            "setup: could not park the marker mid-viewport\nscreen:\n{}",
            harness.screen_contents()
        )
    });
    let screen_before = harness.screen_contents();

    // Setup guards: we must be scrolled into the MIDDLE (bug regime), i.e. the
    // marker is visible while BOTH sentinels are scrolled off. If either of
    // these fails the reproduction is invalid (we'd be at the top or following),
    // so these are SETUP failures, distinct from the position assertion below.
    assert!(
        !screen_before.contains(TOP_SENTINEL),
        "setup: TOP sentinel visible → scroll_offset == 0 (at the absolute top, \
         not the bug regime). Marker row {}.\nscreen:\n{screen_before}",
        before.0
    );
    assert!(
        !screen_before.contains(BOTTOM_SENTINEL),
        "setup: BOTTOM sentinel visible → still pinned to the bottom / following \
         (no jump expected). Marker row {}.\nscreen:\n{screen_before}",
        before.0
    );

    // ── Resize the WIDTH only (rows unchanged) and let prepare_layout rebuild
    // the wrapped row map at the new width.
    harness
        .resize(DEFAULT_ROWS, NARROW_COLS)
        .expect("resize narrower");
    harness.update(Duration::from_millis(900));
    let screen_after = harness.screen_contents();

    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during resize\nscreen:\n{screen_after}"
    );
    assert!(
        !screen_after.contains("panicked"),
        "pager rendered 'panicked' during resize\nscreen:\n{screen_after}"
    );

    // ── The reproduction assertion: the marker must still be visible AND at
    // ~the same viewport row after a width-only resize.
    let (after_row, _) = locate_screen_text(&screen_after, MARKER).unwrap_or_else(|| {
        panic!(
            "REPRO (marker scrolled off after a width-only resize {WIDE_COLS}→{NARROW_COLS} cols): \
             marker was visible at row {} before, gone after. Stale scroll_offset (absolute \
             wrapped-row count) left unchanged across the reflow.\nscreen before:\n{screen_before}\n\
             screen after:\n{screen_after}",
            before.0
        )
    });

    let delta = (after_row as i32 - before.0 as i32).abs();
    assert!(
        delta <= POS_TOLERANCE,
        "REPRO (marker jumped on a width-only resize {WIDE_COLS}→{NARROW_COLS} cols): \
         marker viewport row {} → {} (|Δ|={} > tolerance {}). Re-wrapping the content above the \
         marker grew its height, but scroll_offset (an absolute wrapped-row count) was left \
         unchanged, so the view lost its scroll position.\nscreen before:\n{screen_before}\n\
         screen after:\n{screen_after}",
        before.0,
        after_row,
        delta,
        POS_TOLERANCE
    );

    harness.quit().expect("clean quit");
}
