// Per-test-case module for the `leader_pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 23. **Leader mode — N clients share one session.**
/// Scales the multi-client fan-out to N = 1 driver + `VIEWERS` viewers (bump
/// `VIEWERS` to widen it). The driver spawns the leader and runs a turn; every
/// viewer attaches through the SAME leader and must replay that turn exactly
/// once (duplicated replay history and an empty/stuck pane both fail); a later
/// turn driven from the driver must stream live into ALL viewers exactly once;
/// and the leader + viewers must survive the spawning client's exit.
/// Deliberately NOT a superset of `leader_two_clients_shared_session`: the
/// driver/viewer role-flip (a turn driven from a viewer back to the driver)
/// and the multi-turn scrollback exactly-once integrity stay unique to that
/// test, so it must not be deleted as redundant.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "PTY e2e; run with cargo test -p echo-agent-pager --test leader_pty_e2e -- --ignored --test-threads=1"]
async fn leader_n_clients_shared_session() {
    // N = 1 driver + VIEWERS viewers; bump to scale the live fan-out. Keep N
    // small: worker_threads above and the per-viewer survival pump below are
    // sized for it — raise them together if you scale VIEWERS up.
    const VIEWERS: usize = 2;

    let cluster = LeaderCluster::start(DEFAULT_ROWS, DEFAULT_COLS)
        .await
        .expect("start cluster");
    cluster
        .content()
        .set_response(format!("{} first turn payload.", turn_sentinel(1)));

    // The driver spawns the leader and runs turn 1.
    let mut a = cluster.spawn_leader(&[]).expect("spawn driver");
    a.wait_for_text(WELCOME_SCREEN_SENTINEL, LEADER_TIMEOUT)
        .expect("driver welcome");
    a.inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("driver submit turn 1");
    a.wait_for_text(&turn_sentinel(1), STREAM_TIMEOUT)
        .expect("driver turn 1");

    // Every viewer attaches through the shared leader and must replay the
    // driver's transcript exactly once (duplicated replay or an empty pane
    // both fail).
    let mut viewers: Vec<PtyHarness> = Vec::new();
    for i in 0..VIEWERS {
        let mut v = cluster
            .attach(&[])
            .unwrap_or_else(|e| panic!("spawn viewer {i}: {e}"));
        v.wait_for_text(&turn_sentinel(1), LEADER_TIMEOUT)
            .unwrap_or_else(|e| panic!("viewer {i} replayed driver's transcript: {e}"));
        // Settle the PTY before counting: wait_for_text returns on first match,
        // so a duplicate replay arriving in a later batch would slip past an
        // immediate count. A pump can only reveal a duplicate, never hide one.
        v.update(Duration::from_millis(500));
        let screen = v.screen_contents();
        assert_eq!(
            screen.matches(&turn_sentinel(1)).count(),
            1,
            "turn 1 must appear in viewer {i} exactly once (duplicated replay?)\nscreen:\n{screen}"
        );
        viewers.push(v);
    }

    // Turn 2 driven from the driver streams live into EVERY viewer (fan-out).
    cluster
        .content()
        .set_response(format!("{} second turn payload.", turn_sentinel(2)));
    a.inject_keys(b"again\r").expect("driver submit turn 2");
    a.wait_for_text(&turn_sentinel(2), STREAM_TIMEOUT)
        .expect("driver turn 2");
    for (i, v) in viewers.iter_mut().enumerate() {
        v.wait_for_text(&turn_sentinel(2), STREAM_TIMEOUT)
            .unwrap_or_else(|e| panic!("viewer {i} received driver's live turn: {e}"));
        // Same settle-then-count guard as the replay check, now for the LIVE
        // stream: a duplicated fan-out frame must fail too.
        v.update(Duration::from_millis(500));
        let screen = v.screen_contents();
        assert_eq!(
            screen.matches(&turn_sentinel(2)).count(),
            1,
            "turn 2 must appear in viewer {i} exactly once (duplicated live stream?)\nscreen:\n{screen}"
        );
    }

    // The spawning client's exit must not take the leader (or the viewers)
    // down: each viewer keeps its transcript and stays attached.
    drop(a);
    for (i, v) in viewers.iter_mut().enumerate() {
        v.update(Duration::from_secs(3));
        assert!(
            v.is_running().expect("poll pager liveness"),
            "viewer {i} exited after the driver quit\nscreen:\n{}",
            v.screen_contents()
        );
        assert!(
            !v.contains_text("panicked"),
            "viewer {i} rendered a panic\nscreen:\n{}",
            v.screen_contents()
        );
        assert!(
            v.contains_text(&turn_sentinel(2)),
            "viewer {i} lost its transcript after the driver quit\nscreen:\n{}",
            v.screen_contents()
        );
    }

    for mut v in viewers {
        v.quit().expect("quit viewer");
    }
}
