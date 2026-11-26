// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Ctrl+\ (OpenDashboard). crossterm maps the raw 0x1c byte to Ctrl+4, so the
/// universal dashboard chord must be sent as the kitty CSI-u form: code 92
/// (`\`), modifier 5 (Ctrl). Mirrors `CTRL_ENTER` / `CTRL_SEMICOLON` in common.
const CTRL_BACKSLASH: &[u8] = b"\x1b[92;5u";

/// Attach the (only) agent row as a session overlay from the dashboard list.
/// Down clamps at the last focusable, so three Downs land on the row regardless
/// of the current cursor (fresh open = New Agent button; after a back-out =
/// previously-selected row); Enter then attaches the peeked row. Waits until the
/// overlay is up: the dashboard list ("+ New Agent") is gone and the agent's
/// transcript (MOCKRESPONSE) is shown.
fn attach_overlay(h: &mut PtyHarness) {
    for _ in 0..3 {
        h.inject_keys(keys::DOWN).expect("down to row");
        h.update(Duration::from_millis(200));
    }
    h.inject_keys(keys::ENTER).expect("attach row");
    wait_for_labels_absent(h, &["+ New Agent"], Duration::from_secs(10));
    h.wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(10))
        .expect("attached the original agent as an overlay");
    assert!(
        !h.contains_text("+ New Agent"),
        "attach must leave the dashboard list for the agent overlay\nscreen:\n{}",
        h.screen_contents()
    );
}

/// Dashboard-overlay back-out. Attaching a
/// session lands on the default Prompt focus, so every keyboard back-out path
/// must work and the user must never be trapped:
///   - **Ctrl+\** opens the dashboard from a session (and from inside the overlay);
///   - **empty-prompt Esc** backs out;
///   - **Left on an empty prompt** backs out;
///   - **a drafted-prompt Esc** does NOT back out — it arms "press again to clear";
///   - **Tab then a neutral scrollback Esc** backs out.
/// Each back-out is re-verified against a freshly re-attached overlay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn dashboard_overlay_tab_esc_backout_and_ctrl_backslash() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} dashboard overlay turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered (idle session)");

    // Ctrl+\ opens the dashboard from inside a session (universal back-out).
    harness
        .inject_keys(CTRL_BACKSLASH)
        .expect("ctrl+\\ open dashboard");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("Ctrl+\\ opens the dashboard");

    // ── (c) Drafted-prompt Esc must NOT back out (arms "press again to clear")
    //    and (d) empty-prompt Esc backs out. Overlay lands on Prompt.
    attach_overlay(&mut harness);
    let draft = "OVLDRAFT";
    harness.inject_keys(draft.as_bytes()).expect("type draft");
    harness
        .wait_for_text(draft, Duration::from_secs(10))
        .expect("draft renders in the overlay prompt");
    harness.inject_keys(keys::ESC).expect("esc with draft");
    harness.update(Duration::from_millis(300));
    assert!(
        harness.contains_text("press again to clear"),
        "a drafted overlay prompt Esc must arm clear, not back out\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("+ New Agent"),
        "a drafted overlay prompt Esc must NOT return to the dashboard\nscreen:\n{}",
        harness.screen_contents()
    );
    // Wipe the draft (Ctrl+U clears the armed pending then kills the line).
    harness.inject_keys(b"\x15").expect("ctrl+u clear draft");
    wait_for_labels_absent(&mut harness, &[draft], Duration::from_secs(5));
    // (d) Empty-prompt Esc now backs out to the dashboard.
    harness.inject_keys(keys::ESC).expect("empty-prompt esc");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("empty-prompt overlay Esc must back out to the dashboard");

    // ── (a) Left on an empty prompt backs out. (Left arrow = CSI D.)
    attach_overlay(&mut harness);
    harness
        .inject_keys(b"\x1b[D")
        .expect("left on empty prompt");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("Left on an empty overlay prompt must back out to the dashboard");

    // ── (b) Ctrl+\ from inside the overlay backs out.
    attach_overlay(&mut harness);
    harness
        .inject_keys(CTRL_BACKSLASH)
        .expect("ctrl+\\ from overlay");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("Ctrl+\\ from inside the overlay must back out to the dashboard");

    // ── Tab then a neutral scrollback Esc backs out (bare-scrollback path).
    attach_overlay(&mut harness);
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness.update(Duration::from_millis(400));
    harness
        .inject_keys(keys::ESC)
        .expect("neutral esc back-out");
    harness
        .wait_for_text("+ New Agent", Duration::from_secs(10))
        .expect("Tab then neutral Esc returns to the dashboard list");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
