// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// Minimal mode guards the documented `in_flight_committed` dogfood
/// double-show: a promoted queued prompt's "❯ " block commits (prints) into
/// native scrollback immediately, so cancelling its turn pre-first-token
/// (via Ctrl+C here) must SKIP the composer rewind — a rewind would leave the
/// printed block on screen AND refill the composer, showing the prompt twice.
/// Standard cancel instead: the block renders exactly once and the cancel
/// marker is visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_double_esc_committed_queued_prompt_single_render() {
    const QUEUED_PROMPT: &str = "bravo promoted block";

    let content = ContentController::start().await.expect("start content");
    // Gate turn 1's completion so the queue provably lands mid-turn; turn 2
    // (the promoted prompt's) streams nothing before the cancel thanks to
    // the pacing set just before the release.
    let mut turn_one = content.expect_agent_turn_blocked(
        "running turn before minimal queue promotion",
        "STEPONE first reply.",
    );
    let _turn_two = content.expect_agent_turn(
        "promoted minimal prompt cancelled before first token",
        "STEPTWO never streams before the cancel.",
    );

    let mut harness = spawn_minimal(&content);
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("STEPONE", Duration::from_secs(30))
        .expect("turn 1 streamed (completion still gated)");
    tokio::time::timeout(Duration::from_secs(10), turn_one.wait_blocked())
        .await
        .expect("turn 1 reached completion barrier");

    harness
        .inject_keys(format!("{QUEUED_PROMPT}\r").as_bytes())
        .expect("queue follow-up mid-turn");
    harness
        .wait_for_text("1 queued", Duration::from_secs(10))
        .expect("queue indicator");

    // Promote: turn 1 ends, the queued prompt's block commits (prints) and
    // its turn starts — but its first token is 30s away, the exact window
    // where a naive rewind would double-show the committed block.
    content.set_chunk_delay(Some(Duration::from_secs(30)));
    turn_one.release();
    harness
        .wait_for_full_text(
            &format!("\u{276F} {QUEUED_PROMPT}"),
            Duration::from_secs(30),
        )
        .expect("promoted prompt block committed");
    let committed = harness.full_text().matches(QUEUED_PROMPT).count();
    assert_eq!(
        committed,
        1,
        "committed block must print once\nfull contents:\n{}",
        harness.full_text()
    );

    // Cancel the promoted turn pre-first-token. The committed block forces
    // the standard cancel path (rewind skipped): marker renders, composer
    // stays empty, and the prompt count does NOT grow.
    harness.inject_keys(keys::CTRL_C).expect("Ctrl+C cancel");
    harness
        .wait_for_full_text("Turn cancelled by user", Duration::from_secs(15))
        .expect("standard cancel marker (not a silent rewind)");

    harness.update(Duration::from_millis(500));
    assert_eq!(
        harness.full_text().matches(QUEUED_PROMPT).count(),
        committed,
        "cancel must not re-show the committed prompt (composer refill = double render)\n\
         full contents:\n{}",
        harness.full_text()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    quit_minimal(&mut harness);
}
