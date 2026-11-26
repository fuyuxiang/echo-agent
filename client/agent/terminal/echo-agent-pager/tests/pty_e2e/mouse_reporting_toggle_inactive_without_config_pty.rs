// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Without config/env enablement, scrollback Ctrl+R must not toggle mouse reporting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mouse_reporting_toggle_inactive_without_config_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_mouse_reporting_toggle_config(&content, false);

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    drive_to_scrollback_with_turn(&mut harness, &content).await;

    harness
        .inject_keys(keys::CTRL_R)
        .expect("ctrl-r on scrollback");
    harness.update(Duration::from_millis(800));

    assert!(
        !harness.contains_text(MOUSE_OFF_STICKY),
        "sticky mouse-off banner must not appear without mouse_reporting_toggle\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("Mouse reporting on"),
        "toggle-on toast must not appear without config\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text("panicked"),
        "pager panicked\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
