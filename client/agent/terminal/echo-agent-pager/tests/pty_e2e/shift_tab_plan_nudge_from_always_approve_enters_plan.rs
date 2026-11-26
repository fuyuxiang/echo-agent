// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Always-Approve + plan nudge: one Shift+Tab enters Plan (not Normal).
/// The tip advertises `shift+tab` → plan mode; with the nudge up that chord
/// must jump from Always-Approve rather than taking the next ring step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn shift_tab_plan_nudge_from_always_approve_enters_plan() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} turn done."));

    let binary = pager_binary().expect("resolve pager binary");
    // --yolo/--trust seed Always-Approve; hints env opts the tip in; CWD is
    // the sandboxed content home so trust resolves against the same tree.
    let env_refs = CONTEXTUAL_HINTS_ENV;
    let mut harness = PtyHarness::spawn_with_content_env_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        env_refs,
        Some(content.home()),
    )
    .expect("spawn pager in always-approve");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    // Promote to idle (nudge is gated on session.state.is_idle()).
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");
    harness
        .wait_for_turn_idle(Duration::from_secs(15))
        .expect("turn idle before plan nudge");

    harness
        .inject_keys(b"plan the refactor")
        .expect("type planning keyword");
    harness
        .wait_for_text("plan mode via", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "plan nudge must show; {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    harness.inject_keys(b"\x1b[Z").expect("inject Shift+Tab");
    harness
        .wait_for_text("Switched to mode: Plan", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "nudge + Always-Approve Shift+Tab must enter Plan; {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    assert!(
        !harness.contains_text("Switched to mode: Normal"),
        "must not land on Normal while the plan nudge is showing; screen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
