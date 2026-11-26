// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Drive a session to an idle turn, confirm the prompt owns keys, then Tab to
/// the scrollback and confirm the footer's "Space:prompt" hint appears (the
/// scrollback now owns keys). Shared by the vim-mode and default-config cases.
async fn assert_tab_focuses_scrollback(content: &ContentController) {
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} tab focus turn."));
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");

    // Prompt owns keys before Tab.
    assert!(
        !harness.contains_text("Space:prompt"),
        "precondition: prompt should be focused before Tab\nscreen:\n{}",
        harness.screen_contents()
    );

    // Tab is the leave-prompt / focus-scrollback key (in BOTH modes; Esc is not).
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(5))
        .expect("Tab moves focus to the scrollback");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}

/// Tab focuses scrollback with `[ui].vim_mode = true` (scrollback vim nav on),
/// `[ui].simple_mode = false`. The Esc port left Tab as the focus key
/// independent of vim mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn tab_focuses_scrollback_vim_mode() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "vim_mode = true\nsimple_mode = false");
    assert_tab_focuses_scrollback(&content).await;
}

/// Tab focuses scrollback under the default config (no `[ui]` overrides) —
/// proves the focus key is Tab regardless of the (default-off) vim/simple modes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn tab_focuses_scrollback_default_config() {
    let content = ContentController::start().await.expect("start content");
    assert_tab_focuses_scrollback(&content).await;
}

/// `[ui].simple_mode = true` (non-vim prompt editor) must not change the Esc
/// policy or the Tab focus key — the policy is independent of `simple_mode`.
/// Proves on the real binary: idle Esc Esc clears a draft (clear policy), then
/// Tab (not Esc) focuses the scrollback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn esc_policy_and_tab_focus_work_in_simple_mode() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "simple_mode = true");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} simple-mode turn."));

    // The idle Esc-Esc below needs the widened double-press window.
    let mut harness = spawn_esc_double_press_pager(&content);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle");

    // Esc policy (clear) works the same under simple_mode; the hint wait keeps
    // the presses distinct (`ESC ESC` in one write collapses to one `Esc`).
    let draft = "SIMPLEMODEDRAFT";
    harness.inject_keys(draft.as_bytes()).expect("type draft");
    harness
        .wait_for_text(draft, Duration::from_secs(10))
        .expect("draft renders");
    harness.inject_keys(keys::ESC).expect("first esc");
    harness
        .wait_for_text("press again to clear", Duration::from_secs(15))
        .expect("simple_mode idle Esc must arm the clear confirm");
    harness.inject_keys(keys::ESC).expect("second esc");
    wait_for_labels_absent(&mut harness, &[draft], Duration::from_secs(5));
    assert!(
        !harness.contains_text(draft),
        "simple_mode Esc Esc must clear the draft\nscreen:\n{}",
        harness.screen_contents()
    );

    // Tab (not Esc) is still the focus key under simple_mode.
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(5))
        .expect("Tab focuses scrollback under simple_mode");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
