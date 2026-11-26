//! Built-binary e2e for `StopCancelled`: a `hooks/*.json` key loads, a real subprocess gets the
//! real stdin envelope, and `matcher` filters on `reason`.
//!
//! `#[ignore]`d by default: it needs the echoagent binary (`ECHO_AGENT_BINARY` or a local debug build).
//! `cargo test -p echo-agent-runtime --test test_stop_cancelled_hook_e2e -- --ignored`

use echo_agent_test_support::*;

const MODEL: &str = "chat-completions-model";

#[tokio::test]
#[ignore]
async fn stop_cancelled_hook_fires_with_the_matched_reason() {
    let out = tempfile::TempDir::new().expect("create state dir");
    let server = MockInferenceServer::start_with_models(vec![
        MockModelEntry::new(MODEL).with_api_backend("chat_completions"),
    ])
    .await
    .expect("start mock server");
    server.enqueue_response(
        "/v1/chat/completions",
        scripted::ScriptedResponse::sse(sse::chat_completions_reasoning_then_tool_call_events(
            "let me look",
            "call-1",
            "read_file",
            r#"{"path":"README.md"}"#,
            MODEL,
        )),
    );

    let sandbox = TestSandbox::builder().mock_url(server.url()).git().build();
    let dir = out.path().display();
    let hooks = sandbox.echo_agent_home().join("hooks");
    std::fs::create_dir_all(&hooks).expect("create hooks dir");
    std::fs::write(
        hooks.join("stop_cancelled.json"),
        serde_json::json!({
            "hooks": { "StopCancelled": [
                { "matcher": "max_turns", "hooks": [
                    { "type": "command", "command": format!("cat > {dir}/stdin.json") }] },
                { "matcher": "user_interrupt", "hooks": [
                    { "type": "command", "command": format!("touch {dir}/wrong_matcher_ran") }] },
            ] }
        })
        .to_string(),
    )
    .expect("write hook config");

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args([
        "-p",
        "read the readme",
        "--yolo",
        "--model",
        MODEL,
        "--max-turns",
        "1",
    ])
    .current_dir(sandbox.workspace())
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    let result = run_headless_in_sandbox(cmd, sandbox).await;
    assert!(!result.timed_out, "stderr:\n{}", result.stderr);

    let stdin = out.path().join("stdin.json");
    let text = std::fs::read_to_string(&stdin)
        .unwrap_or_else(|e| panic!("the hook never ran ({e}). stderr:\n{}", result.stderr));
    let envelope: serde_json::Value = serde_json::from_str(&text).expect("hook stdin is JSON");

    assert_eq!(envelope["hookEventName"], "stop_cancelled");
    assert_eq!(envelope["reason"], "max_turns");
    assert!(!out.path().join("wrong_matcher_ran").exists());
}
