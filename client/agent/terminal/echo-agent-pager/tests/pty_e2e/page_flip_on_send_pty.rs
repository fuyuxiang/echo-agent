// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

// Default (unset): send pins the new prompt at the viewport top.
// `[ui] page_flip_on_send = false`: send does not move the viewport.

const TAIL_SENTINEL: &str = "TAILSENTINEL_T1";
const SECOND_PROMPT: &str = "second-prompt-marker";

fn tall_first_response() -> String {
    let mut s = String::from("```\n");
    for i in 0..80 {
        s.push_str(&format!("line {i} payload\n"));
    }
    s.push_str(TAIL_SENTINEL);
    s.push_str("\n```\n");
    s
}

/// Welcome → tall turn 1 → submit turn 2 while holding turn 2 open.
async fn drive_to_second_send(content: &ContentController) -> (PtyHarness, AgentTurnExpectation) {
    let mut first_turn =
        content.expect_agent_turn("page-flip tall first turn", tall_first_response());
    let mut second_turn = content.expect_agent_turn_blocked(
        "page-flip held second turn",
        format!("{MOCK_RESPONSE_SENTINEL} second turn."),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, content, &[])
            .expect("spawn pager");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit first prompt");
    harness
        .wait_for_text(TAIL_SENTINEL, Duration::from_secs(30))
        .expect("turn 1 tail visible");
    tokio::time::timeout(Duration::from_secs(10), first_turn.wait_satisfied())
        .await
        .expect("first turn completes before second send");

    harness
        .inject_keys(format!("{SECOND_PROMPT}\r").as_bytes())
        .expect("submit second prompt");
    harness
        .wait_for_text(SECOND_PROMPT, Duration::from_secs(15))
        .expect("second prompt rendered");
    tokio::time::timeout(Duration::from_secs(10), second_turn.wait_blocked())
        .await
        .expect("second turn reaches completion barrier");
    harness.update(Duration::from_millis(600));
    (harness, second_turn)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn send_page_flips_by_default() {
    let content = ContentController::start().await.expect("start content");
    let (mut harness, second_turn) = drive_to_second_send(&content).await;

    assert!(
        !harness.contains_text(TAIL_SENTINEL),
        "default send should page-flip turn 1's tail off screen\nscreen:\n{}",
        harness.screen_contents()
    );
    let screen = harness.screen_contents();
    let prompt_row = screen
        .lines()
        .position(|l| l.contains(SECOND_PROMPT))
        .expect("second prompt visible");
    assert!(
        prompt_row < (DEFAULT_ROWS as usize) / 2,
        "flipped prompt should be in the top half (row {prompt_row})\nscreen:\n{screen}"
    );

    second_turn.release();
    harness.quit().expect("clean quit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn send_keeps_viewport_when_page_flip_disabled() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "page_flip_on_send = false");
    let (mut harness, second_turn) = drive_to_second_send(&content).await;

    assert!(
        harness.contains_text(TAIL_SENTINEL),
        "page_flip_on_send=false must leave turn 1's tail on screen\nscreen:\n{}",
        harness.screen_contents()
    );

    second_turn.release();
    harness.quit().expect("clean quit");
}
