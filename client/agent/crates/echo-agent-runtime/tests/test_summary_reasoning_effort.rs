//! summary.json reasoning-effort persistence tests.
//!
//! Regression tests for "what effort was this session run on?": a fresh
//! session must record its resolved `reasoning_effort` in `summary.json` at
//! creation — not only after an explicit model/effort switch (the old
//! behavior, which left the field absent for sessions that never switched).
//!
//! Each test spawns a real `echoagent agent stdio` process against a mock
//! inference server and asserts on the persisted `summary.json`.
//!
//! Run locally:
//! ```bash
//! cargo test -p echo-agent-runtime --test test_summary_reasoning_effort -- --ignored
//! ```

use std::future::Future;

use echo_agent_test_support::*;

async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

/// Find `summary.json` for `session_id` under `<home>/.echo-agent/sessions/` and
/// parse it. The sessions tree is `<encoded-cwd>/<session-id>/summary.json`;
/// matching on the directory name avoids re-implementing the cwd encoding.
fn read_summary(home: &std::path::Path, session_id: &str) -> serde_json::Value {
    let sessions_root = home.join(".echo-agent").join("sessions");
    let cwd_dirs = std::fs::read_dir(&sessions_root)
        .unwrap_or_else(|e| panic!("no sessions dir at {}: {e}", sessions_root.display()));
    for cwd_dir in cwd_dirs.flatten() {
        let candidate = cwd_dir.path().join(session_id).join("summary.json");
        if candidate.is_file() {
            let raw = std::fs::read_to_string(&candidate).expect("read summary.json");
            return serde_json::from_str(&raw).expect("parse summary.json");
        }
    }
    panic!(
        "summary.json for session {session_id} not found under {}",
        sessions_root.display()
    );
}

/// A fresh session on a model with a configured reasoning effort must persist
/// that effort in `summary.json` without any model/effort switch.
#[tokio::test]
#[ignore] // requires pre-built binary
async fn test_fresh_session_persists_reasoning_effort() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        let workdir = git_workdir();

        // Configure the mock catalog's model with an explicit effort via the
        // user config override (the same path a remote settings catalog entry or
        // `--effort` would populate).
        let sandbox = TestSandbox::new();
        let echo_agent_dir = sandbox.echo_agent_home();
        std::fs::write(
            echo_agent_dir.join("config.toml"),
            r#"
[model.test-model]
supports_reasoning_effort = true
reasoning_effort = "high"
"#,
        )
        .expect("write config.toml");

        let client =
            EchoAgentStdioClient::spawn_with_sandbox(&server, workdir.workspace(), sandbox).await;
        client.initialize_with_timeout().await;
        let session_id = client
            .create_session_with_timeout(workdir.workspace())
            .await;
        let result = client.prompt_with_timeout(&session_id, "say hello").await;
        assert!(result.is_ok(), "prompt failed: {:?}", result.err());

        let summary = read_summary(client.sandbox().home(), &session_id.0);
        assert_eq!(
            summary.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("high"),
            "fresh session must record its effort in summary.json; got: {summary}"
        );
    })
    .await;
}

/// A fresh session on a model with no configured effort must not invent one:
/// `summary.json` omits the field (the model uses its server-side default).
#[tokio::test]
#[ignore] // requires pre-built binary
async fn test_fresh_session_without_effort_omits_field() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        let workdir = git_workdir();
        let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;

        client.initialize_with_timeout().await;
        let session_id = client
            .create_session_with_timeout(workdir.workspace())
            .await;
        let result = client.prompt_with_timeout(&session_id, "say hello").await;
        assert!(result.is_ok(), "prompt failed: {:?}", result.err());

        let summary = read_summary(client.sandbox().home(), &session_id.0);
        assert_eq!(
            summary.get("reasoning_effort"),
            None,
            "session without a configured effort must omit the field; got: {summary}"
        );
    })
    .await;
}
