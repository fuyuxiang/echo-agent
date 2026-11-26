// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Content-anchored live region: a response that FITS on the
/// screen stays on the visible static band with the prompt directly after it. It
/// is NOT force-pushed to the bottom of the screen — the earlier bottom-pin
/// behavior left a large blank gap *above* a short conversation (the reported
/// regression: "you see a big gap … input snapped to the bottom").
///
/// Discriminating signals (all robust to how the emulator pads blank rows):
/// - the response stays on the visible screen, and is NOT pushed into native
///   scrollback (a response that fits never needs to scroll);
/// - the always-focused prompt (the cursor) sits HIGH on the screen, directly
///   after the short conversation, with the rest of the window left blank below
///   it — bottom-pin would instead put the cursor near the last row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_short_response_stays_on_screen() {
    let content = ContentController::start().await.expect("start content");
    // A short answer — a couple of rendered rows, far shorter than the 50-row
    // screen, so it never needs to scroll into native history.
    content.set_response(format!(
        "{MOCK_RESPONSE_SENTINEL} — short answer that fits."
    ));

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    // Wait for the response to render on the visible screen, then let the turn
    // finish and the commit settle.
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("short response renders on screen");
    harness.update(Duration::from_millis(400));

    let rows = DEFAULT_ROWS;

    // 1) The short response is on the visible screen …
    assert!(
        harness.screen_contents().contains(MOCK_RESPONSE_SENTINEL),
        "short response must stay on the visible screen\nscreen:\n{}",
        harness.screen_contents()
    );
    // 2) … and was NOT force-pushed into native scrollback. Content-anchored: a
    //    response that fits stays put; only content taller than the screen scrolls
    //    (proven separately by `minimal_commits_response_to_scrollback`).
    assert!(
        !harness.scrollback_text().contains(MOCK_RESPONSE_SENTINEL),
        "short response must not be pushed into scrollback\nscrollback:\n{}",
        harness.scrollback_text()
    );

    // 3) The prompt sits directly after the (short) conversation, HIGH on the
    //    screen, with the rest of the window left blank below it — NOT pinned to
    //    the bottom with a big gap above (the regression). The cursor is always
    //    on the focused prompt, so its row is the robust signal: bottom-pin puts
    //    it near `rows - 1`; content-anchored keeps it in the upper portion.
    let (cursor_row, _cursor_col) = harness.cursor_position();
    assert!(
        cursor_row < rows - 12,
        "prompt/cursor should sit high on the screen (content-anchored), not \
         pinned near the bottom: cursor_row={cursor_row}, rows={rows}\nscreen:\n{}",
        harness.screen_contents()
    );

    // 4) Nothing is rendered near the bottom of the screen: the last non-blank
    //    row (the prompt's info bar) is well above the last row. Found explicitly
    //    (not via trailing padding) so the check is independent of how the
    //    emulator represents empty rows.
    let screen = harness.screen_contents();
    let last_non_blank = screen
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i)
        .last()
        .unwrap_or(0);
    assert!(
        last_non_blank < (rows as usize) - 10,
        "content-anchored live region must leave the bottom of the screen blank; \
         last non-blank row was {last_non_blank} of {rows}\nscreen:\n{screen}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
