// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

// ── Auto-compact: the top padding row disappears on tiny terminals ─────────
//
// The default layout reserves a blank top padding row (`outer_vpad`) above the
// status bar. When the terminal is `AUTO_COMPACT_MAX_ROWS` (20) or shorter,
// the render-value compact flag is derived ON (auto-compact) and the padding
// goes away, so the status bar moves to screen row 0. Growing the terminal
// back restores the padding — the user's persisted compact setting never
// changes.
//
// A YAML scenario cannot assert on-screen positions, so this test reads the
// first non-blank screen row directly: row 0 blank = padding present, row 0
// populated (by the status bar) = auto-compact engaged.

/// Short height that engages auto-compact. Above `SHORT_TERMINAL_ROWS` (16)
/// but at most `AUTO_COMPACT_MAX_ROWS` (20), so it pins the auto-compact
/// derivation specifically — not the older short-terminal layout trims.
const SHORT_ROWS: u16 = 18;

/// Index of the first screen row with any non-whitespace content, panicking
/// with the screen when the whole screen is blank.
fn first_content_row(harness: &PtyHarness, when: &str) -> u16 {
    let screen = harness.screen_contents();
    screen
        .lines()
        .position(|line| !line.trim().is_empty())
        .unwrap_or_else(|| panic!("{when}: screen is entirely blank\nscreen:\n{screen}")) as u16
}

/// **Auto-compact drops the top padding row on tiny terminals.**
/// Tall: row 0 is the blank top padding row (first content below it).
/// At `SHORT_ROWS`: auto-compact removes the padding, status bar lands on
/// row 0. Back tall: padding (and the blank row 0) restored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn auto_compact_top_row() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} auto-compact probe."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Enter the agent view (the layout under test) and let the turn end.
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response rendered");
    harness.update(Duration::from_millis(500));

    let tall_row = first_content_row(&harness, "tall spawn");
    assert!(
        tall_row > 0,
        "tall terminal must keep the blank top padding row (content starts below \
         row 0), got first content on row {tall_row}\nscreen:\n{}",
        harness.screen_contents()
    );

    // Shrink to an auto-compact height: the padding row must vanish.
    harness
        .resize(SHORT_ROWS, DEFAULT_COLS)
        .expect("resize short");
    harness.update(Duration::from_millis(900));
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager exited during resize\nscreen:\n{}",
        harness.screen_contents()
    );
    let short_row = first_content_row(&harness, "after shrink");
    assert_eq!(
        short_row,
        0,
        "auto-compact must drop the top padding row at {SHORT_ROWS} rows \
         (status bar on row 0)\nscreen:\n{}",
        harness.screen_contents()
    );

    // Grow back: the derived value reverts to the user's setting.
    harness
        .resize(DEFAULT_ROWS, DEFAULT_COLS)
        .expect("resize tall");
    harness.update(Duration::from_millis(900));
    let restored_row = first_content_row(&harness, "after grow");
    assert!(
        restored_row > 0,
        "growing back must restore the blank top padding row, got first content \
         on row {restored_row}\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.screen_contents().contains("panicked"),
        "pager rendered 'panicked'\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

/// **Auto-compact engages from the startup seed alone.**
/// Spawning already-tiny (no resize event ever fires) must still land the
/// status bar on row 0: the startup seed (`crossterm::terminal::size()` into
/// the initial appearance) is the only path that can have derived it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn auto_compact_at_startup() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} startup probe."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, SHORT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");

    // The welcome menu may be trimmed on a short spawn; the prompt marker
    // always paints. The first char promotes to the agent view under test.
    harness
        .wait_for_text("\u{276f}", WELCOME_TIMEOUT)
        .expect("prompt marker");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response rendered");
    harness.update(Duration::from_millis(500));

    let row = first_content_row(&harness, "tiny spawn");
    assert_eq!(
        row,
        0,
        "a {SHORT_ROWS}-row spawn must start auto-compacted (status bar on \
         row 0, no top padding row) without any resize\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.screen_contents().contains("panicked"),
        "pager rendered 'panicked'\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
