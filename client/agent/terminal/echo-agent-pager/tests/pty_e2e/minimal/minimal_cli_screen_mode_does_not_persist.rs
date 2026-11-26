// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// CLI `--minimal` / `--fullscreen` must not write `[ui] screen_mode` to
/// config.toml. Mode flags are session-scoped; only a manual config.toml edit
/// should make a mode sticky across plain `echoagent` launches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_cli_screen_mode_does_not_persist() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{} no-sticky payload.", turn_sentinel(1)));

    // Sessions are keyed by cwd: both runs must share a stable project dir.
    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    // First run: explicit `--minimal` — must open minimal but not write config.
    let mut first = spawn_minimal_in_dir(&content, DEFAULT_ROWS, DEFAULT_COLS, &[], project.path());
    wait_minimal_ready(&mut first);

    // Give any fire-and-forget persist path time to have written if it still
    // existed, pumping the PTY so the pager never blocks on a full buffer.
    let config_path = content.home().join(".echo-agent").join("config.toml");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        first.update(Duration::from_millis(100));
    }
    let body = std::fs::read_to_string(&config_path).unwrap_or_default();
    assert!(
        !body.contains("screen_mode"),
        "--minimal must not persist [ui] screen_mode; config.toml:\n{body}"
    );
    quit_minimal(&mut first);

    // Second run: NO mode flag. Without a manual config preference the plain
    // launch must open fullscreen (welcome screen), not minimal.
    let binary = pager_binary().expect("resolve pager binary");
    let mut second = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--no-leader"],
        Some(project.path()),
    )
    .expect("spawn plain pager");
    second.set_respond_to_queries(true);

    second
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .unwrap_or_else(|e| {
            panic!(
                "plain echoagent should open fullscreen after --minimal (no sticky write): {e}\nscreen:\n{}",
                second.screen_contents()
            )
        });

    assert!(
        !second.contains_text(MINIMAL_IDLE_SENTINEL),
        "plain launch must not be minimal without config screen_mode\nscreen:\n{}",
        second.screen_contents()
    );
    assert!(
        !second.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        second.screen_contents()
    );

    second.quit().expect("clean quit");
}
