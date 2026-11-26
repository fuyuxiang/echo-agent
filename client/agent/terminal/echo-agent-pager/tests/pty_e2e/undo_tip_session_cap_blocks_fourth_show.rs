// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 18. **Per-session cap — the 4th show is gated.**
/// Within one session the tip shows up to its cap (3). The slot is cleared
/// between shows via TTL expiry (re-showing a still-visible same-key tip only
/// refreshes its TTL and does not re-count), so each fresh show increments the
/// in-memory count. The 4th wipe is then gated → no banner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn undo_tip_session_cap_blocks_fourth_show() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    // Contextual hints ship default-OFF; opt in explicitly so the tip shows.
    let env_refs = CONTEXTUAL_HINTS_ENV;
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        env_refs,
    )
    .expect("spawn");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    // Three fresh shows: wipe → tip → wait out the TTL so the slot clears.
    for i in 1..=3 {
        wipe_substantial_draft(&mut harness);
        harness
            .wait_for_text(UNDO_TIP_SENTINEL, Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("tip must show on wipe #{i}: {e}"));
        // The active tip keeps animation alive, so its TTL ticks down; wait
        // for it to expire (clear the slot) before the next, counted, show.
        wait_for_labels_absent(&mut harness, &[UNDO_TIP_SENTINEL], Duration::from_secs(25));
        assert!(
            !harness.contains_text(UNDO_TIP_SENTINEL),
            "tip should expire via TTL before wipe #{}; screen:\n{}",
            i + 1,
            harness.screen_contents()
        );
    }

    // Fourth wipe: the in-memory count is at the cap (3) → gated, no banner.
    wipe_substantial_draft(&mut harness);
    harness.update(Duration::from_millis(1000));
    assert!(
        !harness.contains_text(UNDO_TIP_SENTINEL),
        "the 4th wipe in a session must be gated by the cap; screen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
