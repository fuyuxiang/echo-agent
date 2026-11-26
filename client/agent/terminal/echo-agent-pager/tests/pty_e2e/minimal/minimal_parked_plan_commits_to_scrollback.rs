// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Taller than the terminal, so a plan held in the live region is clipped.
const PLAN_LINES: usize = 100;

fn missing(harness: &mut PtyHarness, tag: &str) -> Vec<usize> {
    plan_lines_missing(harness, tag, PLAN_LINES)
}

fn duplicated(harness: &mut PtyHarness, tag: &str) -> Vec<usize> {
    plan_lines_duplicated(harness, tag, PLAN_LINES)
}

fn park_plan(
    content: &ContentController,
    harness: &mut PtyHarness,
    dir: &std::path::Path,
    tag: &str,
    call_id: &str,
    prompt: &str,
) -> AgentTurnExpectation {
    std::fs::write(dir.join("plan.md"), plan_body(tag, PLAN_LINES)).expect("seed plan.md");
    let expectation = expect_tool_turn(content, call_id, "exit_plan_mode", "{}".into());
    harness
        .inject_keys(format!("{prompt}\r").as_bytes())
        .expect("submit plan prompt");
    harness
        .wait_for_text(PLAN_PARKED_SENTINEL, Duration::from_secs(60))
        .unwrap_or_else(|e| {
            panic!(
                "plan approval never parked: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    for _ in 0..10 {
        harness.update(Duration::from_millis(100));
    }
    expectation
}

/// Minimal's plan-approval contract: the plan body reaches NATIVE SCROLLBACK
/// **while the approval is parked**, not only once the user answers. Users
/// reported reading a truncated plan with nothing behind it in the scrollback;
/// design doc §6.16.
///
/// Also pins the revision path: a revised plan is a fresh `exit_plan_mode` with
/// a new `tool_call_id`, and must commit as its own block exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_parked_plan_commits_to_scrollback() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} first turn done."));

    // ~5x the screen, so live-region-only rendering is guaranteed to clip.
    let mut harness = spawn_minimal_sized(&content, 20, 100);
    wait_minimal_ready(&mut harness);

    // A first turn, so the session (and its plan.md directory) exists.
    harness.inject_keys(b"go\r").expect("submit first turn");
    harness
        .wait_for_full_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(40))
        .expect("first turn streams");
    let dir = session_dir(&content, &mut harness);

    // ── plan 1, parked ──
    let _first = park_plan(
        &content,
        &mut harness,
        &dir,
        "ONE",
        "call_plan_one",
        "present the plan",
    );
    assert!(
        missing(&mut harness, "ONE").is_empty(),
        "the whole plan must be readable while the approval is parked \
         (missing {:?})\nscrollback rows: {}\nscreen:\n{}",
        missing(&mut harness, "ONE"),
        harness.scrollback_text().lines().count(),
        harness.screen_contents(),
    );
    assert!(
        duplicated(&mut harness, "ONE").is_empty(),
        "parked plan must be printed exactly once (duplicated {:?})",
        duplicated(&mut harness, "ONE"),
    );

    // ── revise: `s` focuses the feedback input, Enter sends it ──
    harness.inject_keys(b"s").expect("request changes");
    harness.update(Duration::from_millis(400));
    let _second = park_plan(
        &content,
        &mut harness,
        &dir,
        "TWO",
        "call_plan_two",
        "make it shorter",
    );
    assert!(
        missing(&mut harness, "TWO").is_empty(),
        "the revised plan must also be readable while parked (missing {:?})",
        missing(&mut harness, "TWO"),
    );
    assert!(
        duplicated(&mut harness, "ONE").is_empty() && duplicated(&mut harness, "TWO").is_empty(),
        "a revision must not re-emit either plan (ONE {:?}, TWO {:?})",
        duplicated(&mut harness, "ONE"),
        duplicated(&mut harness, "TWO"),
    );

    // ── approve ──
    harness.inject_keys(b"a").expect("approve");
    for _ in 0..40 {
        harness.update(Duration::from_millis(100));
    }
    assert!(
        duplicated(&mut harness, "ONE").is_empty() && duplicated(&mut harness, "TWO").is_empty(),
        "approving must not re-print the plan (ONE {:?}, TWO {:?})",
        duplicated(&mut harness, "ONE"),
        duplicated(&mut harness, "TWO"),
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
