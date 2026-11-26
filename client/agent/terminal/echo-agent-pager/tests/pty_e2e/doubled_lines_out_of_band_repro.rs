//! Doubled lines — regression guard via a simulated
//! out-of-band screen reflow.
//!
//! The stack is `tmux -> nvim :terminal -> echoagent`: nvim/tmux repaint echoagent's
//! pane out-of-band (no echoagent PTY resize), and echoagent's diff renderer only
//! re-clears on a real size change — so the rows it doesn't own survive and
//! you get doubled lines at the top/bottom until restart.
//!
//! The harness is a single faithful emulator and can't nest a real tmux/nvim,
//! so we SIMULATE the out-of-band reflow with `feed_screen` (writes straight
//! into the virtual terminal, bypassing echoagent) and then check whether echoagent
//! heals it. `NVIM` is set in echoagent's env so it sees the embedded-editor
//! context the fix keys off (mirroring a real nvim `:terminal`).
//!
//! The fix: echoagent forces a full clear+repaint on `FocusGained` in
//! editor/multiplexer contexts, so the injected row is gone after refocus.
//! The final assertion guards that heal.

use super::common::*;

/// Unique sentinel that echoagent would never render on its own.
const STALE_MARKER: &str = "STALE_OUT_OF_BAND_ROW_ZZZ";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // opt-in PTY e2e (run with `-- --ignored`)
async fn out_of_band_stale_row_heals_on_focus_gained() {
    let content = ContentController::start()
        .await
        .expect("start mock content");

    // Mock-auth env + pretend we're inside a neovim `:terminal` (sets the
    // embedded-editor context the doubled-line fix gates on).
    let overrides: Vec<(String, String)> = vec![(
        "NVIM".into(),
        "/tmp/echo-agent-pty-harness-fake-nvim.sock".into(),
    )];
    let env_refs: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    let binary = pager_binary().expect("resolve pager binary");
    let mut h = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &env_refs,
    )
    .expect("spawn pager");

    h.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome screen");
    // Let the initial draws land before injecting. The Welcome logo shimmers, so
    // the screen never goes fully frame-idle, but the col-1 marker below is safe.
    h.update(Duration::from_millis(800));
    assert!(
        !h.contains_text(STALE_MARKER),
        "marker must be absent before injection"
    );

    // Simulate the out-of-band reflow: write a stale row straight into the
    // virtual screen at col 1 — the static left margin echoagent's diff renderer
    // doesn't repaint during the logo shimmer. The heal is a full clear, so
    // removal is reliable. `\x1b[<row>;<col>H` is 1-based cursor positioning.
    h.feed_screen(format!("\x1b[6;1H{STALE_MARKER}").as_bytes());
    assert!(
        h.contains_text(STALE_MARKER),
        "marker should be on the virtual screen right after injection"
    );

    // echoagent must not self-heal out-of-band content via ordinary diff redraws:
    // it only rewrites cells whose own model changed, so this row is stranded.
    h.update(Duration::from_millis(300));
    assert!(
        h.contains_text(STALE_MARKER),
        "stale row should survive a normal redraw (echoagent's diff renderer doesn't own it)\nscreen:\n{}",
        h.screen_contents()
    );

    // A FocusGained (CSI I) forces a full clear+repaint that re-asserts echoagent's
    // whole screen and removes the out-of-band row.
    h.inject_keys(b"\x1b[I").expect("inject FocusGained");
    // Poll for the heal instead of a fixed settle so host load can't flake it.
    wait_for_labels_absent(&mut h, &[STALE_MARKER], Duration::from_secs(5));
    assert!(
        !h.contains_text(STALE_MARKER),
        "FocusGained should force a full repaint and clear the out-of-band row \
         (regression guard for the doubled-line bug)\nscreen:\n{}",
        h.screen_contents()
    );

    h.quit().expect("clean quit");
}
