// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

const PLAN_LINES: usize = 60;
const TAG: &str = "QUIT";

/// The permanent-loss shape of the parked-plan bug users hit: quitting without
/// answering must still leave the whole plan in the terminal. The pinned live
/// region is repainted and never retained, so only what was committed at park
/// time survives the process. Design doc §6.16.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_parked_plan_survives_quit() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} first turn done."));

    let mut harness = spawn_minimal_sized(&content, 20, 100);
    wait_minimal_ready(&mut harness);
    harness.inject_keys(b"go\r").expect("submit first turn");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(40))
        .expect("first turn streams");

    let dir = session_dir(&content, &mut harness);
    std::fs::write(dir.join("plan.md"), plan_body(TAG, PLAN_LINES)).expect("seed plan.md");

    let _expectation = expect_tool_turn(&content, "call_plan_quit", "exit_plan_mode", "{}".into());
    harness
        .inject_keys(b"present the plan\r")
        .expect("submit plan prompt");
    harness
        .wait_for_text(PLAN_PARKED_SENTINEL, Duration::from_secs(60))
        .expect("plan approval parks");
    for _ in 0..10 {
        harness.update(Duration::from_millis(100));
    }

    // Quit without answering (Ctrl+Q arms, Ctrl+Q confirms).
    let _ = harness.inject_keys(b"\x11");
    harness.update(Duration::from_millis(300));
    let _ = harness.inject_keys(b"\x11");
    for _ in 0..40 {
        harness.update(Duration::from_millis(100));
    }

    let missing = plan_lines_missing(&mut harness, TAG, PLAN_LINES);
    assert!(
        missing.is_empty(),
        "plan must survive in the terminal after quitting while parked \
         (missing {}/{PLAN_LINES}: {missing:?})",
        missing.len(),
    );

    let _ = harness.quit();
}
