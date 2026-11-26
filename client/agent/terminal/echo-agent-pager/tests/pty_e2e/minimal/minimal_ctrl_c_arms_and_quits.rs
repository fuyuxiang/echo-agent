// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal mode has no shortcuts bar, so the double-press quit confirmation must
/// surface under the prompt instead: a first Ctrl+C on an empty, idle prompt
/// arms the quit confirmation and shows "press Ctrl+c again to quit", and a
/// second Ctrl+C within the window exits. (Ctrl+Q / Ctrl+D arm the same way; the
/// hint is rendered by `minimal::live::render_exit_hint`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_ctrl_c_arms_and_quits() {
    let content = ContentController::start().await.expect("start content");
    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    // First Ctrl+C (CSI legacy ETX). Arms quit + shows the hint under the prompt.
    harness.inject_keys(b"\x03").expect("inject Ctrl+C");
    harness
        .wait_for_text("again to quit", Duration::from_secs(5))
        .unwrap_or_else(|e| {
            panic!(
                "quit-confirmation hint expected after first Ctrl+C: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // Second Ctrl+C within the confirm window exits the process.
    harness.inject_keys(b"\x03").expect("inject Ctrl+C again");
    let exit = harness
        .wait_exit_code(Duration::from_secs(5))
        .expect("wait for minimal pager exit");
    assert!(
        matches!(exit, PtyExitPoll::Exited(_) | PtyExitPoll::PendingStatus),
        "second Ctrl+C should quit minimal, got {exit:?}\nscreen:\n{}",
        harness.screen_contents()
    );
}
