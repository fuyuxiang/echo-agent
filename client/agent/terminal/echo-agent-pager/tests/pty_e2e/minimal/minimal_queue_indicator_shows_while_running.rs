// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal status line: pressing Enter with a draft while a turn is
/// running queues the prompt, and the status line reports the pending count
/// ("N queued") together with the `/queue` inspection hint — minimal has no
/// interactive queue pane, so without the hint the count is a dead end.
/// Running `/queue` mid-turn commits the read-only snapshot listing the queued
/// text. Standalone (no leader), so the queue is the client-local
/// pending-prompts list. The queued prompt then promotes and runs once the
/// first turn finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_queue_indicator_shows_while_running() {
    let content = ContentController::start().await.expect("start content");
    // Pace turn 1 so it's still streaming when we queue behind it.
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let _turn_one = content.expect_agent_turn(
        "running turn before minimal queue promotion",
        slow_turn_text("STEPONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "promoted minimal queued prompt",
        "STEPTWO queued prompt handled.",
    );

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("STEPONE", Duration::from_secs(30))
        .expect("turn 1 streaming in the live tail");

    // Enter with a draft while the turn runs → queue it behind the running turn.
    harness
        .inject_keys(b"second queued prompt\r")
        .expect("queue a prompt mid-turn");
    // The count and the inspection hint share the info row: "1 queued · /queue".
    harness
        .wait_for_text("1 queued \u{b7} /queue", Duration::from_secs(10))
        .expect("info row reports the queued count with the /queue hint");

    // `/queue` (slash commands run immediately, they don't queue) commits the
    // read-only snapshot listing the queued prompt.
    inject_keys_paced(&mut harness, b"/queue");
    harness.inject_keys(b"\r").expect("run /queue");
    harness
        .wait_for_full_text("Queued prompt", Duration::from_secs(10))
        .expect("/queue commits the queue snapshot block");
    harness
        .wait_for_full_text("#1  second queued prompt", Duration::from_secs(10))
        .expect("the snapshot lists the queued text");

    // The queued prompt eventually promotes and runs as turn 2.
    harness
        .wait_for_full_text("STEPTWO", Duration::from_secs(40))
        .expect("queued prompt promoted and ran");
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
