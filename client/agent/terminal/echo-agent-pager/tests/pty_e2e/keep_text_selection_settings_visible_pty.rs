// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Seed `[ui] keep_text_selection = "hold"`, open Settings (F2) after a turn,
/// and assert the Mouse row label is visible (modal registration smoke).
/// flash-vs-hold is covered by unit tests (UiConfig / cache / dispatch).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn keep_text_selection_settings_visible_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_keep_text_selection_config(&content);
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} keep selection turn."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("turn rendered");
    harness.update(Duration::from_millis(400));

    // F2 opens settings on AgentScreen (SS3 form used by most terminals).
    const F2: &[u8] = b"\x1bOQ";
    harness.inject_keys(F2).expect("F2 open settings");
    harness.update(Duration::from_millis(500));

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_label = false;
    while Instant::now() < deadline {
        if harness.contains_text("Text selection")
            || harness.contains_text("Flash after copy")
            || harness.contains_text("Hold until dismissed")
        {
            saw_label = true;
            break;
        }
        // Filter search narrows Mouse rows if the viewport is short.
        harness
            .inject_keys(b"/selection")
            .expect("filter selection");
        harness.update(Duration::from_millis(400));
        if harness.contains_text("Text selection")
            || harness.contains_text("Flash after copy")
            || harness.contains_text("Hold until dismissed")
        {
            saw_label = true;
            break;
        }
        harness.inject_keys(keys::ESC).expect("clear filter");
        harness.update(Duration::from_millis(200));
        harness.inject_keys(F2).expect("re-open settings");
        harness.update(Duration::from_millis(400));
    }

    assert!(
        saw_label,
        "settings modal must show Text selection row (config_seeded={})\nscreen:\n{}",
        content
            .home()
            .join(".echo-agent")
            .join("config.toml")
            .exists(),
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
