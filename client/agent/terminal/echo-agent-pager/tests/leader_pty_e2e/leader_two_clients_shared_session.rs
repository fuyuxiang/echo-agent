// Per-test-case module for the `leader_pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 12. **Leader mode — two clients share one session.**
/// The two-terminal flow: pager A starts with `--leader` on an isolated
/// socket (spawning the leader), runs a turn; pager B starts with the same
/// socket + `--resume` and attaches to A's session through the SAME leader.
/// Discriminating for the multi-client replay surface: B must render A's
/// transcript exactly once (duplicated replay history and an empty/stuck
/// pane both fail), later turns must stream live into BOTH panes regardless
/// of which client drives, and the leader + viewer must survive the
/// spawning client's exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "PTY e2e; run with cargo test -p echo-agent-pager --test leader_pty_e2e -- --ignored --test-threads=1"]
async fn leader_two_clients_shared_session() {
    // One shared leader (shared HOME/ECHO_AGENT_HOME hold the sessions AND the
    // explicit leader socket), so B attaches to the leader A spawned instead
    // of the machine's default one.
    let cluster = LeaderCluster::start(DEFAULT_ROWS, DEFAULT_COLS)
        .await
        .expect("start cluster");
    cluster
        .content()
        .set_response(format!("{} first turn payload.", turn_sentinel(1)));

    let mut a = cluster.spawn_leader(&[]).expect("spawn pager A");
    a.wait_for_text(WELCOME_SCREEN_SENTINEL, LEADER_TIMEOUT)
        .expect("A welcome");

    a.inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("A submit turn 1");
    a.wait_for_text(&turn_sentinel(1), STREAM_TIMEOUT)
        .expect("A turn 1");

    // B attaches to A's session (most recent in the shared cwd) via the
    // shared leader and must replay A's transcript.
    let mut b = cluster.attach(&[]).expect("spawn pager B");
    b.wait_for_text(&turn_sentinel(1), LEADER_TIMEOUT)
        .expect("B replayed A's transcript");

    let b_screen = b.screen_contents();
    assert_eq!(
        b_screen.matches(&turn_sentinel(1)).count(),
        1,
        "A's turn must appear in B exactly once (duplicated replay?)\nB screen:\n{b_screen}"
    );

    // Turn 2 driven from A streams live into the attached viewer B.
    cluster
        .content()
        .set_response(format!("{} second turn payload.", turn_sentinel(2)));
    submit_turn(&mut a, "again", &turn_sentinel(2), STREAM_TIMEOUT);
    b.wait_for_text(&turn_sentinel(2), STREAM_TIMEOUT)
        .expect("B received A's live turn");

    // Turn 3 driven from B reaches A (driver/viewer flip).
    cluster
        .content()
        .set_response(format!("{} third turn payload.", turn_sentinel(3)));
    submit_turn(&mut b, "more", &turn_sentinel(3), STREAM_TIMEOUT);
    a.wait_for_text(&turn_sentinel(3), STREAM_TIMEOUT)
        .expect("A received B's live turn");

    // Each submitted prompt top-anchors its turn, scrolling earlier turns
    // above the viewport. Grow the viewport and wheel-scroll to the top
    // (keyboard scroll keys are captured by the focused input box) so the
    // whole 3-turn transcript is on screen, then count every sentinel
    // exactly once — a duplicated replay or a dropped turn both fail here,
    // on either pane.
    //
    // Non-dev builds no longer metronome on tracing_rx; scroll finalize runs
    // via has_active_stream ticks and cadence-suppressed wheels must still
    // schedule ticks (handle_input returns Changed). Bazel/linux amd64 is
    // slower than macOS local: keep retrying wheel bursts until every turn
    // sentinel is visible (or time out), not a single fixed burst budget.
    fn wheel_scroll_to_top(h: &mut PtyHarness) {
        for burst in 0..4 {
            for _ in 0..50 {
                let _ = h.inject_keys(b"\x1b[<64;40;10M");
            }
            h.update(Duration::from_millis(350 + burst * 120));
        }
        h.update(Duration::from_millis(500));
    }

    fn all_turns_once(screen: &str) -> bool {
        (1..=3).all(|turn| screen.matches(&turn_sentinel(turn)).count() == 1)
    }

    for (name, h) in [("A", &mut a), ("B", &mut b)] {
        h.resize(200, DEFAULT_COLS).expect("grow viewport");
        h.update(Duration::from_millis(600));

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut screen = h.screen_contents();
        while !all_turns_once(&screen) && std::time::Instant::now() < deadline {
            wheel_scroll_to_top(h);
            // Home/goto-top via Esc then wheel again if turn 1 still missing.
            if screen.matches(&turn_sentinel(1)).count() == 0 {
                let _ = h.inject_keys(keys::ESC);
                h.update(Duration::from_millis(200));
                let _ = h.inject_keys(keys::ESC);
                h.update(Duration::from_millis(200));
                wheel_scroll_to_top(h);
            }
            screen = h.screen_contents();
        }

        assert!(
            h.is_running().expect("poll pager liveness"),
            "pager {name} exited\nscreen:\n{}",
            h.screen_contents()
        );
        assert!(
            !h.contains_text("panicked"),
            "pager {name} rendered a panic\nscreen:\n{}",
            h.screen_contents()
        );
        let screen = h.screen_contents();
        for turn in 1..=3 {
            assert_eq!(
                screen.matches(&turn_sentinel(turn)).count(),
                1,
                "turn {turn} must appear in {name} exactly once\nscreen:\n{screen}"
            );
        }
    }

    // The spawning client's exit must not take the leader (or B) down: B
    // keeps its transcript and stays attached.
    drop(a);
    b.update(Duration::from_secs(3));
    assert!(
        b.is_running().expect("poll pager liveness"),
        "B exited after A quit\nscreen:\n{}",
        b.screen_contents()
    );
    assert!(
        b.contains_text(&turn_sentinel(3)),
        "B lost its transcript after A quit\nscreen:\n{}",
        b.screen_contents()
    );

    b.quit().expect("quit B");
}
