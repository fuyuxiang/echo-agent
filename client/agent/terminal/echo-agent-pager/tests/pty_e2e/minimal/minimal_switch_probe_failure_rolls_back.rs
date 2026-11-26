// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// An unanswered inline CPR probe rolls `/minimal` back to a working
/// fullscreen session instead of leaving a half-switched terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_switch_probe_failure_rolls_back() {
    let content = ContentController::start().await.expect("start content");
    let sentinel_one = turn_sentinel(1);
    let sentinel_two = turn_sentinel(2);
    content.set_response(format!("{sentinel_one} before failed switch."));

    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let binary = pager_binary().expect("resolve pager binary");
    // Query responses stay DISABLED: the probe timeout is the point.
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--no-leader"],
        Some(project.path()),
    )
    .expect("spawn fullscreen pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 1");
    harness
        .wait_for_text(&sentinel_one, Duration::from_secs(30))
        .expect("turn 1 in fullscreen");

    inject_keys_paced(&mut harness, b"/minimal");
    harness
        .wait_for_text(
            "Switch this session to minimal (scrollback-native) mode",
            Duration::from_secs(5),
        )
        .expect("slash dropdown offers /minimal");
    harness.update(Duration::from_millis(150));
    harness.inject_keys(b"\r").expect("submit /minimal");

    harness
        .wait_for_text("Couldn't switch to minimal mode", Duration::from_secs(30))
        .unwrap_or_else(|e| {
            panic!(
                "rollback explanation missing: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    assert!(
        !harness.contains_text(MINIMAL_IDLE_SENTINEL)
            && !harness.contains_text(MINIMAL_SWITCH_BACK_IDLE_SENTINEL),
        "must not be in minimal mode after a failed switch\nscreen:\n{}",
        harness.screen_contents()
    );

    content.set_response(format!("{sentinel_two} after failed switch."));
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn 2");
    harness
        .wait_for_text(&sentinel_two, Duration::from_secs(30))
        .unwrap_or_else(|e| {
            panic!(
                "fullscreen session broken after rollback: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked after failed switch\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
