// Per-test-case module for the `pty_e2e` integration test crate.
//
// Wire-format coverage for keyboard selection: raw escape sequences are
// injected exactly as terminals emit them, proving the crossterm-parse →
// key-routing → textarea chain per ENCODING — the thing that actually varies
// across terminals. Tiers:
// - `CSI 1;{m}{ABCDHF}` modified arrows and Home/End (m = 1 + shift1/alt2/
//   ctrl4/super8): Terminal.app, iTerm2, VTE, Windows Terminal, tmux passthrough.
// - `CSI {code};{m}u` kitty CSI-u letters: KKP terminals only (Ghostty,
//   Kitty, WezTerm) — the only tier that can carry SUPER, so all Cmd chords
//   live here; non-KKP emulators never produce these bytes (inert, not broken).
// - `ESC [ Z` BackTab and `\t`: everywhere.
// Selections aren't visible in `screen_contents()`, so cases assert through
// selection SEMANTICS: typing over a highlight replaces; after a drop, inserts.
#[allow(unused_imports)]
use super::common::*;

/// Boot to an idle in-session composer (prompt focused, turn finished).
async fn idle_session() -> (ContentController, PtyHarness) {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} done."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");
    // Let post-turn animation settle so the composer is idle and focused.
    harness.update(Duration::from_secs(2));
    (content, harness)
}

/// Type text, apply a selection sequence, type over it, and wait for the result.
///
/// `wiped` is the substring that must disappear — without this, `wait_for_text`
/// can false-pass when the chord only moved the caret and the replacement
/// string is a substring of the unreplaced draft (e.g. "hello X" inside
/// "hello Xworld").
fn select_and_type_over(
    harness: &mut PtyHarness,
    typed: &str,
    selection_seq: &[u8],
    repeat: usize,
    over: &str,
    expect: &str,
    wiped: &str,
) {
    harness.inject_keys(typed.as_bytes()).expect("type draft");
    harness
        .wait_for_text(typed, Duration::from_secs(5))
        .expect("draft echoed");
    for _ in 0..repeat {
        harness.inject_keys(selection_seq).expect("selection keys");
    }
    harness.inject_keys(over.as_bytes()).expect("type over");
    if harness
        .wait_for_text(expect, Duration::from_secs(5))
        .is_err()
    {
        panic!(
            "expected {expect:?} after typing over the selection\nscreen:\n{}",
            harness.screen_contents()
        );
    }
    assert!(
        !harness.contains_text(wiped),
        "selection should have replaced {wiped:?}\nscreen:\n{}",
        harness.screen_contents()
    );
}

/// `CSI 1;2D` (Shift+Left) — the universal tier; five presses select "world".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn legacy_shift_arrow_selects_and_type_replaces() {
    let (_content, mut harness) = idle_session().await;
    select_and_type_over(
        &mut harness,
        "hello world",
        b"\x1b[1;2D",
        5,
        "X",
        "hello X",
        "world",
    );
    harness.quit().expect("clean quit");
}

/// `CSI 1;4D` (Alt+Shift+Left) and `CSI 1;6D` (Ctrl+Shift+Left) word-extends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn legacy_word_extend_selects_alt_and_ctrl_variants() {
    let (_content, mut harness) = idle_session().await;
    select_and_type_over(
        &mut harness,
        "alpha beta",
        b"\x1b[1;4D",
        1,
        "X",
        "alpha X",
        "beta",
    );
    // Ctrl+C on a non-empty idle draft clears it.
    harness.inject_keys(b"\x03").expect("clear draft");
    select_and_type_over(
        &mut harness,
        "gamma delta",
        b"\x1b[1;6D",
        1,
        "Y",
        "gamma Y",
        "delta",
    );
    harness.quit().expect("clean quit");
}

/// `CSI 1;2H` / `CSI 1;2F` (Shift+Home / Shift+End) — row-edge extends that
/// work WITHOUT the Kitty protocol, unlike Cmd+Shift+arrows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn legacy_shift_home_end_select_to_row_edges() {
    let (_content, mut harness) = idle_session().await;

    // Shift+Home from the end selects the whole draft.
    select_and_type_over(
        &mut harness,
        "WIPEME01",
        b"\x1b[1;2H",
        1,
        "RDONE01",
        "RDONE01",
        "WIPEME01",
    );
    assert!(
        !harness.contains_text("WIPEME01"),
        "Shift+Home selection should have been replaced\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\x03").expect("clear draft");

    // Plain Home (CSI H), then Shift+End selects forward to the row end.
    harness.inject_keys(b"WIPEME02").expect("type draft");
    harness
        .wait_for_text("WIPEME02", Duration::from_secs(5))
        .expect("draft echoed");
    harness.inject_keys(b"\x1b[H").expect("Home");
    harness.inject_keys(b"\x1b[1;2F").expect("Shift+End");
    harness.inject_keys(b"RDONE02").expect("type over");
    harness
        .wait_for_text("RDONE02", Duration::from_secs(5))
        .expect("replacement echoed");
    assert!(
        !harness.contains_text("WIPEME02"),
        "Shift+End selection should have been replaced\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}

/// `CSI 1;2A` (Shift+Up) across a multiline draft (Alt+Enter = ESC + CR).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn legacy_shift_up_selects_across_lines() {
    let (_content, mut harness) = idle_session().await;
    harness.inject_keys(b"one").expect("line 1");
    harness.inject_keys(b"\x1b\r").expect("Alt+Enter newline");
    harness.inject_keys(b"two").expect("line 2");
    harness
        .wait_for_text("two", Duration::from_secs(5))
        .expect("second line echoed");
    // Shift+Up: head moves from end of "two" to end of "one" — selects "\ntwo".
    harness.inject_keys(b"\x1b[1;2A").expect("Shift+Up");
    harness.inject_keys(b"X").expect("type over");
    harness
        .wait_for_text("oneX", Duration::from_secs(5))
        .expect("selection across lines replaced");
    harness.quit().expect("clean quit");
}

/// KKP tier: `CSI 1;10D` (Cmd+Shift+Left) row-extend, then CSI-u `CSI 120;9u`
/// (Cmd+X) cut and `CSI 99;9u` (Cmd+C) copy-keeping-highlight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn kitty_super_chords_row_extend_cut_and_copy() {
    let (_content, mut harness) = idle_session().await;

    // Cmd+Shift+Left extends to the row start; Cmd+X cuts the highlight.
    harness.inject_keys(b"CUTME9Z").expect("type draft");
    harness
        .wait_for_text("CUTME9Z", Duration::from_secs(5))
        .expect("draft echoed");
    harness.inject_keys(b"\x1b[1;10D").expect("Cmd+Shift+Left");
    harness.inject_keys(b"\x1b[120;9u").expect("Cmd+X");
    harness.inject_keys(b"AFTERCUT").expect("type after cut");
    harness
        .wait_for_text("AFTERCUT", Duration::from_secs(5))
        .expect("post-cut text echoed");
    assert!(
        !harness.contains_text("CUTME9Z"),
        "Cmd+X should have cut the highlighted draft\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\x03").expect("clear draft");

    // Cmd+C keeps the highlight (browser semantics): the next char replaces.
    harness.inject_keys(b"COPYME8Y").expect("type draft");
    harness
        .wait_for_text("COPYME8Y", Duration::from_secs(5))
        .expect("draft echoed");
    harness.inject_keys(b"\x1b[1;10D").expect("Cmd+Shift+Left");
    harness.inject_keys(b"\x1b[99;9u").expect("Cmd+C");
    harness
        .inject_keys(b"REPLACED7")
        .expect("type over kept highlight");
    harness
        .wait_for_text("REPLACED7", Duration::from_secs(5))
        .expect("replacement echoed");
    assert!(
        !harness.contains_text("COPYME8Y"),
        "Cmd+C must keep the highlight so the next char replaces it\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}

/// `ESC [ Z` (BackTab): one press must BOTH cycle the mode AND drop the
/// highlight — e2e pin for the registry-bypass fix (Bugbot finding).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn backtab_with_selection_cycles_mode_and_drops_highlight() {
    let (_content, mut harness) = idle_session().await;
    harness.inject_keys(b"alpha beta").expect("type draft");
    harness
        .wait_for_text("alpha beta", Duration::from_secs(5))
        .expect("draft echoed");
    harness.inject_keys(b"\x1b[1;4D").expect("Alt+Shift+Left");
    harness.inject_keys(b"\x1b[Z").expect("BackTab");
    harness
        .wait_for_text("Switched to mode: Plan", Duration::from_secs(10))
        .expect("mode cycled on the same press");
    harness.inject_keys(b"X").expect("type after BackTab");
    // The highlight was dropped, so "X" INSERTS at the former head (start of
    // "beta" after a leftward extension) instead of replacing the word — a
    // surviving highlight would produce "alpha X" with "beta" gone.
    harness
        .wait_for_text("alpha Xbeta", Duration::from_secs(5))
        .expect("char inserted at the head, not replaced — highlight was dropped");
    harness.quit().expect("clean quit");
}
