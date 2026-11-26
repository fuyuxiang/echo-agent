// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 21. **Queue + send-now lifecycle.**
/// One realistic journey: turn 1 streams; queue P1 and P2; remove P1;
/// send I1 now via the chord (cancel-and-send: turn 1 is cancelled silently
/// and I1 runs as its own turn); P2 promotes as the following turn. The
/// final request's user-message sequence must be exactly [prompt, I1 (with
/// the interjection preamble), P2] with P1 absent everywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn queue_and_interjection_lifecycle() {
    let content = ContentController::start().await.expect("start content");
    // Gate turn 1's terminal event so the ENTIRE mid-turn setup — queue P1
    // and P2, remove P1, refocus the prompt, type + chord I1 — provably lands
    // while turn 1 is still the running turn, even under heavy suite load.
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before queue lifecycle send-now",
        slow_turn_text("STEPONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "lifecycle sent-now message",
        "STEPTWO sent-now message acknowledged.",
    );
    let _turn_three = content.expect_agent_turn(
        "remaining lifecycle queued prompt",
        "STEPTHREE promoted prompt handled.",
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
        .wait_for_text("STEPONE", Duration::from_secs(45))
        .expect("step 1: turn streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness.inject_keys(b"lifecycle p-one\r").expect("queue P1");
    harness
        .wait_for_text("lifecycle p-one", Duration::from_secs(20))
        .expect("step 2: P1 queued");
    harness.inject_keys(b"lifecycle p-two\r").expect("queue P2");
    harness
        .wait_for_text("lifecycle p-two", Duration::from_secs(20))
        .expect("step 3: P2 queued");

    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("focus queue pane");
    harness.update(Duration::from_millis(300));
    harness.inject_keys(b"x").expect("remove P1");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while harness.contains_text("lifecycle p-one") {
        assert!(
            std::time::Instant::now() < deadline,
            "step 4 failed: removed P1 still on screen\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }
    // Return focus to the prompt for the send-now.
    harness
        .inject_keys(CTRL_SEMICOLON)
        .expect("unfocus queue pane");
    harness.update(Duration::from_millis(200));

    harness
        .inject_keys(b"lifecycle i-one")
        .expect("type send-now message");
    harness.inject_keys(CTRL_ENTER).expect("send-now chord");
    turn_one.release();
    // Cancel-and-send: turn 1 is cancelled silently; I1 commits as a
    // standard "❯ " prompt block and runs as its own turn.
    // I1 (send-now) then P2 drain back-to-back. The "❯ lifecycle i-one"
    // promotion and the intermediate STEPTWO reply are scrolled above the
    // viewport by P2's start-adoption before a 100ms poll can observe them, so
    // gate on the FINAL reply (stable at the viewport head) and prove the
    // [prompt, I1, P2] order + send-now silence via the recorded wire below.
    harness
        .wait_for_text("STEPTHREE", Duration::from_secs(90))
        .expect("steps 5-7: I1 then P2 drained through to the final reply");

    // The send-now cancel of turn 1 is silent.
    assert!(
        !harness.contains_text("Turn cancelled by user"),
        "send-now cancel must not render a cancelled marker\nscreen:\n{}",
        harness.screen_contents()
    );

    let bodies = content.request_bodies();
    let last = bodies.last().expect("final request recorded");
    // User-role context preambles (user_info, skill reminders) don't carry
    // <user_query>; real prompts do.
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
        finals[1].contains("lifecycle i-one") && finals[1].contains(INTERJECTION_WIRE_PREFIX),
        "second must be I1 with the interjection preamble: {finals:#?}"
    );
    assert!(
        finals[2].contains("lifecycle p-two") && !finals[2].contains(INTERJECTION_WIRE_PREFIX),
        "third must be the naturally drained P2 with no send-now preamble: {finals:#?}"
    );
    assert!(
        !last.to_string().contains("lifecycle p-one"),
        "removed P1 reached the wire"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
