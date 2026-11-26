// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// `/minimal` from a fullscreen session switches in process: no re-exec, no
/// resume replay; history commits into native scrollback plus a seam marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_slash_switches_from_fullscreen() {
    let content = ContentController::start().await.expect("start content");
    let sentinel = turn_sentinel(1);
    content.set_response(format!("{sentinel} fullscreen payload."));

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
        .expect("submit turn");
    harness
        .wait_for_text(&sentinel, Duration::from_secs(30))
        .expect("mock response in fullscreen");

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
        .wait_for_text(MINIMAL_SWITCH_BACK_IDLE_SENTINEL, Duration::from_secs(45))
        .unwrap_or_else(|e| {
            panic!(
                "/minimal did not switch this session to minimal mode: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
    harness
        .wait_for_full_text(&sentinel, Duration::from_secs(30))
        .unwrap_or_else(|e| {
            panic!(
                "prior turn must be committed after the in-process /minimal switch: {e}\nfull:\n{}",
                harness.full_text()
            )
        });
    harness
        .wait_for_full_text("Switched to minimal mode", Duration::from_secs(15))
        .unwrap_or_else(|e| {
            panic!(
                "switch marker block missing: {e}\nfull:\n{}",
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
        "pager panicked after /minimal\nscreen:\n{}",
        harness.screen_contents()
    );

    quit_minimal(&mut harness);
}
