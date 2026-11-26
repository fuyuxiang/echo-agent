// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 2b. **Shift+Tab on the welcome screen starts a session in Plan mode.**
/// Pressing Shift+Tab (BackTab, `ESC [ Z`) before typing anything must
/// leave the welcome screen, create a session, and cycle the mode —
/// the transient "Switched to mode: Plan" banner proves both halves:
/// the key was promoted to a new session (welcome → agent view) AND
/// the forwarded BackTab resolved to `Action::CycleMode` pre-session.
/// Cycle with the auto gate on: Normal → Plan → Auto → … Re-enables the
/// sandbox-pinned gate and pins the starting mode (an unconfigured home
/// would otherwise launch straight into Auto).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn shift_tab_on_welcome_starts_session_in_plan_mode() {
    let content = ContentController::start().await.expect("start content");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--permission-mode", "default"],
        &[("ECHO_AGENT_AUTO_PERMISSION_MODE", "1")],
    )
    .expect("spawn pager with content");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    // Shift+Tab → BackTab (CSI Z).
    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");

    harness
        .wait_for_text("Switched to mode: Plan", Duration::from_secs(10))
        .expect("plan mode banner after Shift+Tab on welcome screen");

    // Second press cycles Plan → Auto (gate defaults ON).
    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");
    harness
        .wait_for_text("Switched to mode: Auto", Duration::from_secs(10))
        .expect("auto banner on second Shift+Tab");

    harness.quit().expect("clean quit");
}
