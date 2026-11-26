// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// A prompt queued behind a running turn survives `/minimal` (the queue is
/// client memory the legacy re-exec dropped).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_switch_preserves_queued_prompt() {
    let content = ContentController::start().await.expect("start content");
    let turn_one = content.expect_agent_turn_blocked(
        "running turn across the mode switch",
        slow_turn_text("QTURNONE"),
    );
    let _turn_two = content.expect_agent_turn(
        "queued follow-up promoted after the switch",
        "QTURNTWO queued follow-up answered.",
    );

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
        .wait_for_text("QTURNONE", Duration::from_secs(30))
        .expect("turn 1 streaming (completion gated)");

    inject_keys_paced(&mut harness, b"queued follow-up prompt");
    harness.inject_keys(b"\r").expect("queue follow-up");
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
        .wait_for_full_text("Switched to minimal mode", Duration::from_secs(45))
        .expect("in-process switch landed");

    turn_one.release();
    harness
        .wait_for_full_text("QTURNTWO", Duration::from_secs(45))
        .unwrap_or_else(|e| {
            panic!(
                "queued prompt did not survive the switch: {e}\nfull:\n{}",
                harness.full_text()
            )
        });

    // "Reopening session…" only prints on the legacy exec path.
    assert!(
        !harness.full_text().contains("Reopening session"),
        "in-process /minimal must not re-exec; found reopen text:\n{}",
        harness.full_text()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
