// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// With two mid-turn queued rows, empty Enter sends the **top** (first) row
/// now — not the most recently typed one. Cancel-and-send: the running turn
/// is cancelled silently, alpha runs as its own next turn (with the interjection
/// preamble), and bravo stays queued to promote afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn empty_enter_sends_top_not_last_of_two() {
    let content = ContentController::start().await.expect("start content");
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before top-row send-now",
        slow_turn_text("TURNONE"),
    );
    let mut turn_two =
        content.expect_agent_turn("top queued row", "TURNTWO top-row send-now acknowledged.");
    let mut turn_three = content.expect_agent_turn(
        "remaining queued row",
        "TURNTHREE remaining queue promoted.",
    );

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
        .wait_for_text("TURNONE", Duration::from_secs(45))
        .expect("turn 1 streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(b"queue-alpha-top\r")
        .expect("queue alpha");
    harness
        .wait_for_text("queue-alpha-top", Duration::from_secs(20))
        .expect("alpha visible");
    harness
        .inject_keys(b"queue-bravo-later\r")
        .expect("queue bravo");
    harness
        .wait_for_text("queue-bravo-later", Duration::from_secs(20))
        .expect("bravo visible");

    harness
        .inject_keys(b"\r")
        .expect("empty Enter send-now top");
    turn_one.release();
    // Alpha (the promoted TOP row) then bravo drain back-to-back after the
    // completion release. Each promoted "❯ …" block and every reply —
    // including the final TURNTHREE — can scroll above the viewport before a
    // 100ms poll observes it, so gating on any on-screen marker is inherently
    // racy (a flaky observation, not a real failure — same rationale as
    // `removed_queued_prompt_never_sent`). Gate on the WIRE instead: wait
    // until bravo's request has been sent, which is the authoritative record
    // that both queued rows drained in order. Pump the event loop while
    // waiting so the queued rows actually promote.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !all_user_messages(&content)
        .iter()
        .any(|u| u.contains("queue-bravo-later"))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "queued rows never drained through to the final turn\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }
    tokio::time::timeout(Duration::from_secs(10), turn_two.wait_satisfied())
        .await
        .expect("top queued row expectation satisfied");
    tokio::time::timeout(Duration::from_secs(10), turn_three.wait_satisfied())
        .await
        .expect("remaining queued row expectation satisfied");

    // The send-now cancel of turn 1 is silent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    let users = all_user_message_blobs(&content);
    let alpha = users
        .iter()
        .find(|u| u.contains("queue-alpha-top"))
        .unwrap_or_else(|| panic!("top row never on wire: {users:#?}"));
    assert!(
        alpha.contains(INTERJECTION_WIRE_PREFIX),
        "send-now must use the interjection preamble: {alpha}"
    );
    assert!(
        alpha.contains("<user_query>"),
        "send-now must wrap the steered text in user_query: {alpha}"
    );

    // The final request's user sequence proves the order: prompt, then the
    // TOP row (alpha), then bravo — never bravo before alpha.
    let bodies = content.request_bodies();
    let last = bodies.last().expect("final request recorded");
    let finals: Vec<String> = last["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("<user_query>"))
        })
        .map(|m| m["content"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(3, finals.len(), "expected 3 user messages: {finals:#?}");
    assert!(finals[0].contains(PROMPT), "first: {finals:#?}");
    assert!(
        finals[1].contains("queue-alpha-top"),
        "second must be the TOP row: {finals:#?}"
    );
    assert!(
        finals[2].contains("queue-bravo-later") && !finals[2].contains(INTERJECTION_WIRE_PREFIX),
        "third must be the naturally drained bravo with no send-now preamble: {finals:#?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
