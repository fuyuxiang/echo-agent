// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Rewind is SKIPPED when a queued prompt exists: Ctrl+C on the running turn
/// is a standard cancel (visible marker, A never mixed into the composer),
/// the queued B promotes as the next turn, and each of A/B renders exactly
/// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ctrlc_with_queued_prompt_no_dup() {
    const PROMPT_A: &str = "alpha primary task";
    const PROMPT_B: &str = "bravo queued follow";

    let content = ContentController::start().await.expect("start content");
    // Gate turn A's terminal event so the queue + Ctrl+C provably land
    // mid-turn (the cancel abort beats the held completion).
    let mut turn_a = content
        .expect_agent_turn_blocked("running turn A before cancel", slow_turn_text("ALPHARESP"));
    let _turn_b = content.expect_agent_turn(
        "queued turn B promoted after cancel",
        "BRAVORESP promoted after cancel.",
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT_A}\r").as_bytes())
        .expect("submit A");
    harness
        .wait_for_text("ALPHARESP", Duration::from_secs(45))
        .expect("A streaming");
    tokio::time::timeout(Duration::from_secs(10), turn_a.wait_blocked())
        .await
        .expect("turn A reached completion barrier");

    harness
        .inject_keys(format!("{PROMPT_B}\r").as_bytes())
        .expect("queue B mid-turn");
    harness
        .wait_for_text(PROMPT_B, Duration::from_secs(20))
        .expect("B visible as a queued row");

    harness.inject_keys(keys::CTRL_C).expect("Ctrl+C cancel A");
    turn_a.release();

    // Standard cancel (queued prompts skip the rewind): A is cancelled and B
    // promotes as the next turn. The "Turn cancelled by user" marker and the
    // "❯ B" promotion scroll above the viewport once B's start-adoption pins
    // its block to the head, so gate on B's reply (stable at the head) and
    // prove correctness via the composer state + the recorded wire below.
    harness
        .wait_for_text("BRAVORESP", Duration::from_secs(90))
        .expect("B promoted and replied");

    // A stayed a committed block — never mixed into the composer.
    assert!(
        !composer_holds(&harness, PROMPT_A),
        "cancel with a queued prompt must not restore A to the composer\nscreen:\n{}",
        harness.screen_contents()
    );
    // No duplication: A (cancelled) and B (promoted) each reach the wire
    // exactly once in the final request — A is not interrupt-dup'd and B is not
    // double-sent. A visible-line count is unreliable here (A scrolls above the
    // viewport on B's adoption), so assert against the wire.
    let bodies = content.request_bodies();
    let last = bodies.last().expect("final request recorded");
    let user_queries: Vec<String> = last["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .filter(|c| c.contains("<user_query>"))
        .map(str::to_owned)
        .collect();
    let wire_count = |needle: &str| user_queries.iter().filter(|c| c.contains(needle)).count();
    assert_eq!(
        wire_count(PROMPT_A),
        1,
        "A must reach the wire exactly once (no interrupt dup): {user_queries:#?}"
    );
    assert_eq!(
        wire_count(PROMPT_B),
        1,
        "B must reach the wire exactly once: {user_queries:#?}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
