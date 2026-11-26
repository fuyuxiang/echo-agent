#[allow(unused_imports)]
use super::common::*;

const DESCRIPTION: &str = "agent fold output sentinel";
const MIDDLE_SENTINEL: &str = "AGENTROW06";

fn double_click_text(harness: &mut PtyHarness, needle: &str) {
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, needle)
        .unwrap_or_else(|| panic!("locate {needle:?}; screen:\n{screen}"));
    let click = format!(
        "{}{}",
        sgr_mouse(0, row, col, 'M'),
        sgr_mouse(0, row, col, 'm'),
    );
    for _ in 0..2 {
        harness
            .inject_keys(click.as_bytes())
            .expect("inject SGR click");
        harness.update(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
async fn agent_execute_full_output_double_click_fold_pty() {
    let content = ContentController::start().await.expect("start content");
    seed_ui_config(&content, "keep_text_selection = \"flash\"");
    let args = json!({
        "command": "printf 'AGENTROW%02d\\n' $(seq 1 12)",
        "description": DESCRIPTION,
    })
    .to_string();
    let _tool_turn = expect_tool_turn(
        &content,
        "call_agent_full_output_fold",
        "run_terminal_command",
        args,
    );
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} command finished."));

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &["--yolo", "--trust"],
        Some(content.home()),
    )
    .expect("spawn pager");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(45))
        .expect("tool turn settled");

    assert!(
        !harness.contains_text("AGENTROW01") && !harness.contains_text(MIDDLE_SENTINEL),
        "agent execute must remain collapsed after completion\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\t").expect("focus scrollback");
    harness
        .wait_for_text("Ctrl+e:", Duration::from_secs(10))
        .expect("scrollback owns keys");
    double_click_text(&mut harness, DESCRIPTION);
    harness
        .wait_for_text(MIDDLE_SENTINEL, Duration::from_secs(15))
        .unwrap_or_else(|_| {
            panic!(
                "double-click must reveal full agent output; screen:\n{}",
                harness.screen_contents()
            )
        });

    harness.update(Duration::from_millis(500));
    double_click_text(&mut harness, DESCRIPTION);
    harness
        .wait_for_text_absent(MIDDLE_SENTINEL, Duration::from_secs(15))
        .unwrap_or_else(|_| {
            panic!(
                "second double-click must collapse agent output; screen:\n{}",
                harness.screen_contents()
            )
        });

    harness.quit().expect("clean quit");
}
