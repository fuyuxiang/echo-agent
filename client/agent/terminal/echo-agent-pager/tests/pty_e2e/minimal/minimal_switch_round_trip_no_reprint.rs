// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// A fullscreen⇄minimal round trip retains the committed frontier: the second
/// minimal stint must not re-print blocks already committed by the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_switch_round_trip_no_reprint() {
    let content = ContentController::start().await.expect("start content");
    let sentinel_one = turn_sentinel(1);
    let sentinel_two = turn_sentinel(2);
    content.set_response(format!("{sentinel_one} first payload."));

    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--no-leader"],
        Some(project.path()),
    )
    .expect("spawn fullscreen pager");
    // Unanswered CPR probes abort the in-process switch to minimal.
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 1");
    harness
        .wait_for_text(&sentinel_one, Duration::from_secs(30))
        .expect("turn 1 in fullscreen");

    let run_switch = |harness: &mut PtyHarness, cmd: &[u8], dropdown_row: &str| {
        inject_keys_paced(harness, cmd);
        harness
            .wait_for_text(dropdown_row, Duration::from_secs(5))
            .expect("slash dropdown row");
        harness.update(Duration::from_millis(150));
        harness.inject_keys(b"\r").expect("submit switch command");
    };

    run_switch(
        &mut harness,
        b"/minimal",
        "Switch this session to minimal (scrollback-native) mode",
    );
    harness
        .wait_for_text(MINIMAL_SWITCH_BACK_IDLE_SENTINEL, Duration::from_secs(45))
        .expect("first switch to minimal");
    harness
        .wait_for_full_text(&sentinel_one, Duration::from_secs(30))
        .expect("turn 1 committed in minimal stint 1");

    content.set_response(format!("{sentinel_two} second payload."));
    run_switch(
        &mut harness,
        b"/fullscreen",
        "Switch this session to fullscreen mode",
    );
    harness
        .wait_for_text("Switched to fullscreen mode", Duration::from_secs(30))
        .expect("switch back to fullscreen");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 2");
    harness
        .wait_for_text(&sentinel_two, Duration::from_secs(30))
        .expect("turn 2 in fullscreen");

    run_switch(
        &mut harness,
        b"/minimal",
        "Switch this session to minimal (scrollback-native) mode",
    );
    harness
        .wait_for_text(MINIMAL_SWITCH_BACK_IDLE_SENTINEL, Duration::from_secs(45))
        .expect("second switch to minimal");
    harness
        .wait_for_full_text(&sentinel_two, Duration::from_secs(30))
        .expect("turn 2 committed in minimal stint 2");

    // Alt-screen frames never enter emulator scrollback, so a second copy can
    // only come from a duplicate commit.
    let full = harness.full_text();
    let occurrences = full.matches(&sentinel_one).count();
    assert_eq!(
        occurrences, 1,
        "turn 1 must not re-commit on the second minimal stint\nfull:\n{full}"
    );

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked during round trip\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
