// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

// ── Interactive flow e2e tests ──────────────────────────────────────────

/// 15. **In-session Shift+Tab cycles permission mode.**
/// Routes BackTab through the agent view's `resolve_action`, the path that
/// previously dropped `CycleMode`; test 2b only covers the welcome screen.
/// With the auto gate on: Normal → Plan → Auto → Always-Approve → Normal.
/// Re-enables the sandbox-pinned gate and pins the starting mode (an
/// unconfigured home would otherwise launch straight into Auto).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn shift_tab_in_session_cycles_mode() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} turn done."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--permission-mode", "default"],
        &[("ECHO_AGENT_AUTO_PERMISSION_MODE", "1")],
    )
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

    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");
    harness
        .wait_for_text("Switched to mode: Plan", Duration::from_secs(10))
        .expect("first cycle: Normal -> Plan");

    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");
    harness
        .wait_for_text("Switched to mode: Auto", Duration::from_secs(10))
        .expect("second cycle: Plan -> Auto");

    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");
    harness
        .wait_for_text("Switched to mode: Always-Approve", Duration::from_secs(10))
        .expect("third cycle: Auto -> Always-Approve");

    harness.inject_keys(b"\x1b[Z").expect("inject BackTab");
    harness
        .wait_for_text("Switched to mode: Normal", Duration::from_secs(10))
        .expect("fourth cycle: Always-Approve -> Normal (full loop)");

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
