//! PTY e2e tests for the runtime XTVERSION probe, run with:
//! `cargo test -p echo-agent-pager --test pty_xtversion -- --ignored --nocapture`

use std::time::Duration;

use echo_agent_pager_pty_harness::{PtyHarness, pager_binary};

const ROWS: u16 = 50;
const COLS: u16 = 120;
const WELCOME_TIMEOUT: Duration = Duration::from_secs(20);
const WELCOME_SCREEN_SENTINEL: &str = "Quit";

/// The XTVERSION query bytes the pager emits at startup.
const XTVERSION_QUERY: &[u8] = b"\x1b[>0q";

/// Forces brand detection to `Unknown` regardless of the runner's own
/// terminal; empty values are treated as absent by the pager's `env_get`.
const UNKNOWN_BRAND_ENV: &[(&str, &str)] = &[
    ("TERM_PROGRAM", ""),
    ("TERM_PROGRAM_VERSION", ""),
    ("TERMINAL_EMULATOR", ""),
    ("WEZTERM_VERSION", ""),
    ("ITERM_SESSION_ID", ""),
    ("ITERM_PROFILE", ""),
    ("TERM_SESSION_ID", ""),
    ("KITTY_WINDOW_ID", ""),
    ("ALACRITTY_SOCKET", ""),
    ("VTE_VERSION", ""),
    ("WT_SESSION", ""),
    ("VSCODE_GIT_ASKPASS_MAIN", ""),
    ("CURSOR_TRACE_ID", ""),
    ("TMUX", ""),
    ("TMUX_PANE", ""),
    ("STY", ""),
    ("ZELLIJ", ""),
    ("ZELLIJ_SESSION_NAME", ""),
];

/// Headline safety property: a mishandled probe/reply must never render
/// as typed garbage on screen.
fn assert_no_probe_garbage_on_screen(harness: &PtyHarness) {
    for fragment in [">|PtyHarnessTerm", "[?62", "[>0q"] {
        assert!(
            !harness.contains_text(fragment),
            "probe/reply fragment {fragment:?} leaked onto the rendered screen"
        );
    }
}

fn raw_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Pump until `needle` appears in the raw PTY byte stream or timeout.
fn wait_for_raw_bytes(harness: &mut PtyHarness, needle: &[u8], timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        harness.update(Duration::from_millis(20));
        if raw_contains(harness.raw_output(), needle) {
            return true;
        }
    }
    false
}

/// Unknown brand → probe fires; the harness's scripted reply is surfaced
/// in `/doctor`, never as screen garbage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_probe_round_trip() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an unknown terminal"
    );

    // Answer as a fictional terminal: XTVERSION DCS reply + DA1 reply.
    harness
        .inject_keys(b"\x1bP>|PtyHarnessTerm 9.9\x1b\\")
        .expect("inject XTVERSION reply");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    assert_no_probe_garbage_on_screen(&harness);

    // Surface check: /doctor shows the probed identity.
    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("PtyHarnessTerm 9.9", Duration::from_secs(10))
        .expect("XTVERSION identity shown in /doctor");

    assert!(!harness.contains_text("panicked"));
    harness.quit().expect("clean quit");
}

/// Allowlisted brand (`TERM_PROGRAM=WezTerm`) → query written, reply
/// surfaced. Env scrub + override so the runner's own markers can't flip
/// the gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn allowlisted_brand_probe_fires() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut env = UNKNOWN_BRAND_ENV.to_vec();
    env.push(("TERM_PROGRAM", "WezTerm"));
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], &env, None).expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an allowlisted brand"
    );
    harness
        .inject_keys(b"\x1bP>|PtyHarnessTerm 9.9\x1b\\")
        .expect("inject XTVERSION reply");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    assert_no_probe_garbage_on_screen(&harness);

    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("PtyHarnessTerm 9.9", Duration::from_secs(10))
        .expect("XTVERSION identity shown for an allowlisted brand");

    assert!(!harness.contains_text("panicked"));
    harness.quit().expect("clean quit");
}

/// Non-allowlisted brand (`TERM_PROGRAM=vscode`) → no query written,
/// regardless of whatever else is in the runner's env (deliberately no scrub).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn non_allowlisted_brand_skips_probe() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::new_inherited_env(
        &binary,
        ROWS,
        COLS,
        &[],
        &[("TERM_PROGRAM", "vscode")],
        None,
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    assert!(
        !raw_contains(harness.raw_output(), XTVERSION_QUERY),
        "pager emitted an XTVERSION query for a non-allowlisted brand"
    );

    harness.quit().expect("clean quit");
}

/// Multiplexer detected (TMUX set) → no query written: the innermost
/// layer would answer as itself, which the multiplexer field already
/// records. Later env entries override the UNKNOWN_BRAND_ENV scrub.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn multiplexer_skips_probe() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut env = UNKNOWN_BRAND_ENV.to_vec();
    env.push(("TMUX", "/tmp/tmux-1000/default,12345,0"));
    env.push(("TMUX_PANE", "%0"));
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], &env, None).expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    assert!(
        !raw_contains(harness.raw_output(), XTVERSION_QUERY),
        "pager emitted an XTVERSION query inside a multiplexer"
    );

    harness.quit().expect("clean quit");
}

/// Silent terminal (no XTVERSION, no DA1) → clean startup after the
/// deadline, no hang, no xtversion line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_no_reply_starts_cleanly() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text despite silent terminal");

    assert!(!harness.contains_text("panicked"));
    assert_no_probe_garbage_on_screen(&harness);

    // /doctor must omit the xtversion line entirely.
    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("Environment", Duration::from_secs(10))
        .expect("doctor output");
    assert!(
        !harness.contains_text("xtversion"),
        "xtversion line should be absent when the terminal never replied"
    );

    harness.quit().expect("clean quit");
}

/// Unterminated DCS reply + DA1 → stalled fragment dropped by the event
/// filter, no identity, no garbage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_malformed_reply_is_discarded() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an unknown terminal"
    );

    // Unterminated DCS reply: a stalled fragment the filter must drop.
    harness
        .inject_keys(b"\x1bP>|PtyHarnessTerm 9.9")
        .expect("inject malformed reply");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text despite malformed reply");
    assert!(!harness.contains_text("panicked"));
    assert_no_probe_garbage_on_screen(&harness);

    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("Environment", Duration::from_secs(10))
        .expect("doctor output");
    assert!(
        !harness.contains_text("xtversion"),
        "malformed reply must not produce an xtversion line"
    );

    harness.quit().expect("clean quit");
}

/// Late reply (~1s after startup, well past any blocking window) → still
/// swallowed by the event filter and recorded, never rendered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_late_reply_swallowed_and_recorded() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an unknown terminal"
    );

    // Answer ~1s after the query — far past any blocking read, inside the
    // filter's arm window (anchored on query emission: welcome render can
    // exceed the window under parallel-test load).
    harness.update(Duration::from_millis(1000));
    harness
        .inject_keys(b"\x1bP>|PtyHarnessTerm 9.9\x1b\\")
        .expect("inject late XTVERSION reply");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    assert_no_probe_garbage_on_screen(&harness);

    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("PtyHarnessTerm 9.9", Duration::from_secs(10))
        .expect("late XTVERSION identity shown in /doctor");

    assert!(!harness.contains_text("panicked"));
    harness.quit().expect("clean quit");
}

/// Keystrokes typed around the reply survive; the reply does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_keystrokes_interleaved_with_reply() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an unknown terminal"
    );

    // Interleave right after the query so the filter is provably armed
    // (welcome render can exceed the arm window under parallel-test load).
    harness.inject_keys(b"he").expect("type before reply");
    harness.update(Duration::from_millis(50));
    harness
        .inject_keys(b"\x1bP>|PtyHarnessTerm 9.9\x1b\\")
        .expect("inject XTVERSION reply");
    harness.update(Duration::from_millis(50));
    harness.inject_keys(b"y").expect("type after reply");

    harness
        .wait_for_text("hey", Duration::from_secs(10))
        .expect("typed keystrokes survive the reply");
    assert_no_probe_garbage_on_screen(&harness);

    assert!(!harness.contains_text("panicked"));
    harness.quit().expect("clean quit");
}

/// Reply split across writes (slow trickling link) → still detected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn unknown_brand_split_reply_round_trip() {
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::new_inherited_env(&binary, ROWS, COLS, &[], UNKNOWN_BRAND_ENV, None)
            .expect("spawn pager");

    assert!(
        wait_for_raw_bytes(&mut harness, XTVERSION_QUERY, WELCOME_TIMEOUT),
        "pager never emitted the XTVERSION query for an unknown terminal"
    );

    harness
        .inject_keys(b"\x1bP>|PtyHarness")
        .expect("inject reply first half");
    harness.update(Duration::from_millis(100));
    harness
        .inject_keys(b"Term 9.9\x1b\\")
        .expect("inject reply second half");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    assert_no_probe_garbage_on_screen(&harness);

    harness.inject_keys(b"/doctor\r").expect("run /doctor");
    harness
        .wait_for_text("PtyHarnessTerm 9.9", Duration::from_secs(10))
        .expect("split XTVERSION reply shown in /doctor");

    assert!(!harness.contains_text("panicked"));
    harness.quit().expect("clean quit");
}
