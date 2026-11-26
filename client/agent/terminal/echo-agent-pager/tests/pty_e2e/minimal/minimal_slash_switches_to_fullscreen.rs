// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use crate::common::*;

/// `/fullscreen` from a minimal session switches in place (no re-exec); the
/// in-memory conversation renders in the alt-screen scrollback pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn minimal_slash_switches_to_fullscreen() {
    let content = ContentController::start().await.expect("start content");
    let sentinel = turn_sentinel(1);
    content.set_response(format!("{sentinel} minimal payload."));

    let project = tempfile::tempdir().expect("create project dir");
    std::fs::create_dir_all(project.path().join(".git")).expect("create .git");

    let mut harness =
        spawn_minimal_in_dir(&content, DEFAULT_ROWS, DEFAULT_COLS, &[], project.path());
    wait_minimal_ready(&mut harness);

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit turn");
    harness
        .wait_for_full_text(&sentinel, Duration::from_secs(30))
        .expect("turn committed in minimal");

    inject_keys_paced(&mut harness, b"/fullscreen");
    harness
        .wait_for_text(
            "Switch this session to fullscreen mode",
            Duration::from_secs(5),
        )
        .expect("slash dropdown offers /fullscreen");
    harness.update(Duration::from_millis(150));
    harness.inject_keys(b"\r").expect("submit /fullscreen");

    // The sentinel is already visible pre-switch, so the transition signal is
    // the minimal idle status disappearing while history stays present.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        harness.update(Duration::from_millis(100));
        let screen = harness.screen_contents();
        let left_minimal = !screen.contains(MINIMAL_IDLE_SENTINEL)
            && !screen.contains(MINIMAL_SWITCH_BACK_IDLE_SENTINEL)
            && !screen.contains("Switch this session to fullscreen mode");
        let history_present = screen.contains(&sentinel) || harness.full_text().contains(&sentinel);
        if left_minimal && history_present {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "/fullscreen did not leave minimal mode with history intact\nscreen:\n{}\nfull:\n{}",
                harness.screen_contents(),
                harness.full_text()
            );
        }
    }

    assert!(
        !harness.contains_text("panicked"),
        "pager panicked after /fullscreen\nscreen:\n{}",
        harness.screen_contents()
    );

    // "Reopening session…" only prints on the legacy exec path.
    assert!(
        !harness.full_text().contains("Reopening session"),
        "in-process /fullscreen must not re-exec; found reopen text:\n{}",
        harness.full_text()
    );

    harness
        .wait_for_text("Switched to fullscreen mode", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "fullscreen switch hint missing: {e}\nscreen:\n{}",
                harness.screen_contents()
            )
        });

    // Mode switches are session-scoped: must not write `[ui] screen_mode`.
    let config_path = content.home().join(".echo-agent").join("config.toml");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(100));
    }
    let body = std::fs::read_to_string(&config_path).unwrap_or_default();
    assert!(
        !body.contains("screen_mode"),
        "/fullscreen must not persist [ui] screen_mode; config.toml:\n{body}"
    );

    harness.quit().expect("clean quit");
}
