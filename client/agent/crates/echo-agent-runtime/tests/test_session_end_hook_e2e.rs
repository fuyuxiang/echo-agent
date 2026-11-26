//! Built-binary e2e: SessionEnd hooks fire on headless process exit.
//!
//! Regression for the non-leader quit path that used to cancel the agent
//! without flushing session actors, so SessionEnd never ran on `/exit` /
//! `echoagent -p` exit.
//!
//! `#[ignore]`d by default — needs the echoagent binary (`ECHO_AGENT_BINARY` or a local
//! debug build):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_session_end_hook_e2e -- --ignored
//! ```
//!
//! CI coverage of the same machinery without a built binary lives in
//! `echo_agent_runtime::agent::activity` tests (the flush quiesce loop and its
//! grace expiry) and `echo_agent_pager::acp::spawn` tests (the worker join:
//! clean exit, worker error, panic rendering, and the abandon-at-budget
//! branch this e2e cannot reach).

use echo_agent_test_support::*;

/// Runs headless with a SessionEnd hook that writes stdin + a marker file.
async fn run_with_session_end_hook() -> (HeadlessResult, MockInferenceServer, tempfile::TempDir) {
    let state_dir = tempfile::TempDir::new().expect("create state dir");
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let sandbox = TestSandbox::builder().mock_url(server.url()).git().build();

    let state = state_dir.path().display();
    let script_path = sandbox.home().join("session_end_hook.sh");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\n\
             cat > {state}/stdin.json\n\
             touch {state}/marker\n\
             exit 0\n"
        ),
    )
    .expect("write hook script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hook script");
    }

    let hooks_dir = sandbox.echo_agent_home().join("hooks");
    std::fs::create_dir_all(&hooks_dir).expect("create hooks dir");
    std::fs::write(
        hooks_dir.join("session_end.json"),
        serde_json::json!({
            "hooks": {
                "SessionEnd": [{
                    "hooks": [{
                        "type": "command",
                        "command": format!("sh {}", script_path.display()),
                        "timeout": 30
                    }]
                }]
            }
        })
        .to_string(),
    )
    .expect("write hook config");

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(["-p", "say hello", "--yolo"])
        .current_dir(sandbox.workspace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let result = run_headless_in_sandbox(cmd, sandbox).await;

    (result, server, state_dir)
}

#[tokio::test]
#[ignore]
async fn session_end_hook_fires_on_headless_exit() {
    let (result, server, state_dir) = run_with_session_end_hook().await;
    assert_headless_success(&result, "session_end hook e2e", Some(&server));

    let marker = state_dir.path().join("marker");
    assert!(
        marker.is_file(),
        "SessionEnd hook must write a marker on process exit (non-leader flush path); \
         missing {marker:?}. stderr:\n{}",
        result.stderr
    );

    let stdin_path = state_dir.path().join("stdin.json");
    let text = std::fs::read_to_string(&stdin_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", stdin_path.display()));
    let envelope: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("hook stdin not JSON: {e}\n{text}"));

    let event = envelope["hookEventName"]
        .as_str()
        .unwrap_or_else(|| panic!("hookEventName missing: {envelope}"));
    assert!(
        event == "session_end" || event == "SessionEnd",
        "expected SessionEnd event name, got {event:?}"
    );

    // `reason` is an already-shipped part of the hook payload that user scripts
    // match on: `shutdown` is emitted by the `SessionCommand::Shutdown` arm
    // (leader auto-update / relaunch today), `channel_closed` by the actor's
    // channel-closed arm. This change adds no new value — it routes non-leader
    // exits through the existing Shutdown command — so renaming `shutdown` to
    // something narrower here would break those scripts. Future distinct causes
    // (e.g. a signal-driven or idle-eviction end) should be added as new values
    // alongside it.
    let reason = envelope["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("reason missing: {envelope}"));
    assert_eq!(
        reason, "shutdown",
        "flush path should send SessionCommand::Shutdown (reason=shutdown), got {reason:?}"
    );
}
