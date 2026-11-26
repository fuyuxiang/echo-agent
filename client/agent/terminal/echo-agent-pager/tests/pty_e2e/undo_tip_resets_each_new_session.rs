// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 16. **Per-session COUNT reset — the key new-behavior test.**
/// The seen count is in-memory (not persisted), so each pager run starts at 0.
/// Run 1 drives the count to its cap (3 TTL-spaced shows) and confirms a 4th
/// wipe is then gated. Run 2 reuses the SAME `$HOME` and does ONE wipe: the tip
/// MUST appear — which only holds if the count reset to 0. (A persisted count
/// sitting at the cap would suppress Run 2, so this genuinely proves the reset,
/// not merely that a "permanently disabled" flag wasn't saved.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn undo_tip_resets_each_new_session() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    // Same env (same $HOME TempDir) for both spawns. Contextual hints ship
    // default-OFF, so opt in explicitly or the undo tip never shows.
    let env_refs = CONTEXTUAL_HINTS_ENV;

    // Run 1: drive the in-memory seen count to its cap (3 TTL-spaced shows),
    // so the count is exhausted before quitting. Each new show needs the
    // previous banner to expire via its ~3s TTL first — re-wiping while it is
    // still visible only refreshes the TTL without incrementing the count.
    {
        let mut harness = PtyHarness::spawn_with_content_env(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            &content,
            &[],
            env_refs,
        )
        .expect("spawn run 1");
        harness
            .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome run 1");
        for i in 1..=3 {
            wipe_substantial_draft(&mut harness);
            harness
                .wait_for_text(UNDO_TIP_SENTINEL, Duration::from_secs(10))
                .unwrap_or_else(|e| panic!("tip must show on run-1 wipe #{i}: {e}"));
            // Wait out the TTL so the slot clears and the next show counts.
            wait_for_labels_absent(&mut harness, &[UNDO_TIP_SENTINEL], Duration::from_secs(25));
            assert!(
                !harness.contains_text(UNDO_TIP_SENTINEL),
                "run-1 tip should expire via TTL before wipe #{}",
                i + 1
            );
        }
        // Cap reached: a further wipe shows nothing for the rest of this run.
        wipe_substantial_draft(&mut harness);
        harness.update(Duration::from_millis(1000));
        assert!(
            !harness.contains_text(UNDO_TIP_SENTINEL),
            "run-1 cap reached: the 4th wipe must be gated; screen:\n{}",
            harness.screen_contents()
        );
        harness.quit().expect("quit run 1");
    }

    // Run 2: SAME $HOME. A persisted cap would suppress the tip here;
    // per-session in-memory state means it shows again.
    {
        let mut harness = PtyHarness::spawn_with_content_env(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            &content,
            &[],
            env_refs,
        )
        .expect("spawn run 2");
        harness
            .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome run 2");
        wipe_substantial_draft(&mut harness);
        harness
            .wait_for_text(UNDO_TIP_SENTINEL, Duration::from_secs(10))
            .expect("tip MUST show again in a fresh session (nothing persisted)");
        harness.quit().expect("quit run 2");
    }
}
