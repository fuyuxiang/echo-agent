// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The `reasoning_efforts` menu is settable from the client config TOML, not
/// just the server: a `[model.<id>]` override renders in `/effort`, independent
/// of what the server sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn reasoning_efforts_from_config_toml_menu() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("echo-agent-4.5").with_supports_reasoning_effort(true),
    ])
    .await
    .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} turn."));

    // Seed `~/.echo-agent/config.toml` with a per-model reasoning-effort menu.
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create .echo-agent");
    // Quote the dotted model id: bare `[model.echo_agent-4.5]` is TOML key-path syntax (model.echo_agent-4.5), not the id "echo-agent-4.5".
    std::fs::write(
        echo_agent_home.join("config.toml"),
        "[model.\"echo-agent-4.5\"]\nreasoning_efforts = [{ value = \"high\", label = \"ConfigHigh\" }]\n",
    )
    .expect("write config.toml");

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

    inject_keys_paced(&mut harness, b"/effort ");
    harness
        .wait_for_text("ConfigHigh", Duration::from_secs(10))
        .expect("config-driven label in /effort dropdown");
    assert!(
        !harness.contains_text("Extended reasoning"),
        "config list must replace the built-in rows\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
