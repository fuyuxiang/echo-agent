// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// After queuing a follow-up mid-turn, the ephemeral tip advertises send-now
/// (`… to send now`). Opt into contextual hints explicitly so the tip cannot
/// be soft-disabled by remote defaults in CI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn send_now_tip_after_mid_turn_queue() {
    let content = ContentController::start().await.expect("start content");
    content.set_chunk_delay(Some(Duration::from_millis(150)));
    let _turn_one = content.expect_agent_turn(
        "running turn while send-now tip appears",
        slow_turn_text("TURNONE"),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let env_refs = CONTEXTUAL_HINTS_ENV;
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        env_refs,
    )
    .expect("spawn pager with contextual hints");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text("TURNONE", Duration::from_secs(30))
        .expect("turn 1 streaming");

    harness
        .inject_keys(b"follow up after tip\r")
        .expect("queue follow-up");
    harness
        .wait_for_text(SEND_NOW_TIP_SENTINEL, Duration::from_secs(10))
        .expect("send-now tip must show after mid-turn queue");
    assert!(
        harness.contains_text("Queued"),
        "tip should include Queued copy; screen:\n{}",
        harness.screen_contents()
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );
    harness.quit().expect("clean quit");
}
