//! End-to-end test for the global `[models]` defaults.
//!
//! Runs the built echoagent binary against the mock inference server with a
//! caller-owned `$ECHO_AGENT_HOME` whose `config.toml` sets every global `[models]`
//! default. Asserts the turn succeeds with all of them set and that the
//! wire-observable one — `extra_headers` — reaches the `/v1/chat/completions`
//! request header, for a model with no per-model `[model.<id>]` override.
//!
//! The scalar defaults (temperature, top_p, max_completion_tokens, max_retries,
//! inference_idle_timeout_secs, stream_tool_calls) are exercised here to prove
//! they parse and the turn still completes; their resolution onto the model is
//! covered directly by `config.rs` unit tests. The headless turn does not
//! surface sampling params in the chat-completions body, so they are not
//! wire-asserted here.
//!
//! `#[ignore]` (needs a built binary). Run locally (auto-builds the pager):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_global_extra_headers_e2e -- --ignored
//! ```

use echo_agent_test_support::*;

/// Every global `[models]` default is accepted, and the wire-observable
/// `extra_headers` reaches the inference request with no per-model block in play.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn global_models_config_reaches_inference_request() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let sandbox = TestSandbox::builder().mock_url(server.url()).build();

    let echo_agent_home = sandbox.echo_agent_home().to_path_buf();
    std::fs::write(
        echo_agent_home.join("config.toml"),
        r#"[models]
extra_headers = { "X-Request-Tags" = "team=example,env=prod" }
temperature = 0.5
top_p = 0.25
max_completion_tokens = 4096
max_retries = 7
inference_idle_timeout_secs = 600
stream_tool_calls = true
"#,
    )
    .expect("write config.toml");

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(["-p", "say hi", "--yolo", "--output-format", "json"])
        .arg("--cwd")
        .arg(workdir.workspace())
        .current_dir(workdir.workspace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let result = run_headless_in_sandbox(cmd, sandbox).await;
    assert_headless_success(&result, "global models config e2e", Some(&server));

    let requests = server.requests();
    let chat = requests
        .iter()
        .find(|e| e.method == "POST" && e.path.contains("chat/completions"))
        .unwrap_or_else(|| {
            panic!(
                "no POST /v1/chat/completions request logged; requests:\n{}",
                server.request_log_summary()
            )
        });
    assert_eq!(
        chat.header("x-request-tags"),
        Some("team=example,env=prod"),
        "global [models].extra_headers must reach the request header; requests:\n{}",
        server.request_log_summary()
    );
}
