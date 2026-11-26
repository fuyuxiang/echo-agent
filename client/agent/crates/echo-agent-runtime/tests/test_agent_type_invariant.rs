//! Agent-type invariant integration tests.
//!
//! These tests exercise the full shell lifecycle via ACP stdio against a mock
//! inference server, verifying that `agent_type = f(model)` holds across:
//!
//! - Session creation with default models
//! - Zero-turn model switching (harness rebuild)
//! - Mid-session model switching (rejection)
//! - Same-type model switching (no rebuild)
//! - Session resume
//!
//! Each test spawns a real `echoagent agent stdio` process, speaks the full ACP
//! protocol, and asserts on the inference request bodies (system prompt) and
//! stderr tracing output to verify the correct harness was used.
//!
//! Run locally:
//! ```bash
//! cargo test -p echo-agent-runtime --test test_agent_type_invariant -- --ignored
//! ```
use echo_agent_test_support::*;
use std::future::Future;
async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}
/// Delete the shell's on-disk models cache so the next process is forced to
/// re-fetch from the mock server's `/v1/models` endpoint. Without this, the
/// second spawn in a resume test reads the stale cache written by phase 1
/// and never sees the updated model list.
fn invalidate_models_cache(home: &std::path::Path) {
    let cache = home.join(".echo-agent").join("models_cache.json");
    if cache.exists() {
        std::fs::remove_file(&cache).expect("failed to delete models_cache.json");
    }
}
/// Start a mock server with two models:
/// - `default-model`: no agent_type (→ defaults to "echo-agent-build")
async fn dual_model_server() -> MockInferenceServer {
    MockInferenceServer::start_with_models(vec![
        MockModelEntry::new("default-model"),
        MockModelEntry::with_agent_type("cursor-model", "cursor"),
    ])
    .await
    .expect("start mock server")
}
/// Start a mock server with two models that share the same agent_type:
/// - `model-a`: no agent_type (→ "echo-agent-build")
/// - `model-b`: no agent_type (→ "echo-agent-build")
async fn same_type_server() -> MockInferenceServer {
    MockInferenceServer::start_with_models(vec![
        MockModelEntry::new("model-a"),
        MockModelEntry::new("model-b"),
    ])
    .await
    .expect("start mock server")
}
/// Session created with a model that has no `agent_type` should use the
/// `echo-agent-build` harness. The system prompt sent to the LLM should contain
/// the echo-agent-build identity string.
#[tokio::test]
#[ignore]
async fn test_default_model_uses_echo_agent_build_harness() {
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
        let sys_prompt = server
            .last_system_prompt()
            .expect("should have at least one inference request");
        assert!(
            sys_prompt.contains("EchoAgent") || sys_prompt.contains("echoagent"),
            "default model should use echo-agent-build harness\nsystem prompt preview: {}",
            &sys_prompt[..sys_prompt.len().min(500)]
        );
    })
    .await;
}
/// Switching between two models with the same agent_type should succeed
/// without a harness rebuild.
#[tokio::test]
#[ignore]
async fn test_same_type_model_switch_no_rebuild() {
    with_local_set(|| async {
        let server = same_type_server().await;
        let workdir = git_workdir();
        let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;
        client.initialize_with_timeout().await;
        let session_id = client
            .create_session_with_model_timeout(workdir.workspace(), "model-a")
            .await;
        let result = client.prompt_with_timeout(&session_id, "say hello").await;
        assert!(result.is_ok(), "first prompt failed: {:?}", result.err());
        let switch_result = client.set_model_with_timeout(&session_id, "model-b").await;
        assert!(
            switch_result.is_ok(),
            "same-type model switch should succeed\nerror: {:?}\nstderr: {}",
            switch_result.err(),
            stderr_tail(&client.stderr(), 2000)
        );
        let result2 = client.prompt_with_timeout(&session_id, "say goodbye").await;
        assert!(
            result2.is_ok(),
            "second prompt after model switch failed: {:?}",
            result2.err()
        );
    })
    .await;
}
/// A session created with the default model, persisted, and reloaded should
/// still use the same harness — the system prompt in the resumed session's
/// first inference request should match the original.
#[tokio::test]
#[ignore]
async fn test_session_resume_preserves_harness() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        let workdir = git_workdir();
        let mut writer = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;
        writer.initialize_with_timeout().await;
        let session_id = writer
            .create_session_with_timeout(workdir.workspace())
            .await;
        let result = writer.prompt_with_timeout(&session_id, "say hello").await;
        assert!(result.is_ok(), "prompt failed: {:?}", result.err());
        let original_sys_prompt = server
            .last_system_prompt()
            .expect("should have captured system prompt");
        let shared_sandbox = writer.take_sandbox();
        invalidate_models_cache(shared_sandbox.home());
        drop(writer);
        let reader =
            EchoAgentStdioClient::spawn_with_sandbox(&server, workdir.workspace(), shared_sandbox)
                .await;
        reader.initialize_with_timeout().await;
        let _ = reader
            .load_session_with_timeout(&session_id, workdir.workspace())
            .await;
        let result2 = reader.prompt_with_timeout(&session_id, "say goodbye").await;
        assert!(
            result2.is_ok(),
            "resumed prompt failed: {:?}",
            result2.err()
        );
        let resumed_sys_prompt = server
            .last_system_prompt()
            .expect("should have captured resumed system prompt");
        let original_has_echo_agent =
            original_sys_prompt.contains("EchoAgent") || original_sys_prompt.contains("echoagent");
        let resumed_has_echo_agent =
            resumed_sys_prompt.contains("EchoAgent") || resumed_sys_prompt.contains("echoagent");
        assert_eq!(
            original_has_echo_agent,
            resumed_has_echo_agent,
            "resumed session should use the same harness as the original\n\
             original identity markers: echoagent={original_has_echo_agent}\n\
             resumed identity markers: echoagent={resumed_has_echo_agent}\n\
             original prompt (first 300): {}\n\
             resumed prompt (first 300): {}",
            &original_sys_prompt[..original_sys_prompt.len().min(300)],
            &resumed_sys_prompt[..resumed_sys_prompt.len().min(300)],
        );
    })
    .await;
}
/// A model that doesn't declare `agent_type` in its metadata should
/// default to `"echo-agent-build"`. This exercises the serde default.
#[tokio::test]
#[ignore]
async fn test_model_without_agent_type_defaults_to_echo_agent_build() {
    with_local_set(|| async {
            let server = MockInferenceServer::start_with_models(
                    vec![
            MockModelEntry::new("no-agent-type-model"),
        ],
                )
                .await
                .expect("start mock server");
            let workdir = git_workdir();
            let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;
            client.initialize_with_timeout().await;
            let session_id = client
                .create_session_with_model_timeout(
                    workdir.workspace(),
                    "no-agent-type-model",
                )
                .await;
            let result = client.prompt_with_timeout(&session_id, "say hello").await;
            assert!(result.is_ok(), "prompt failed: {:?}", result.err());
            let sys_prompt = server
                .last_system_prompt()
                .expect("should have at least one inference request");
            assert!(
            sys_prompt.contains("EchoAgent") || sys_prompt.contains("echoagent"),
            "model without agent_type should default to echo-agent-build harness\nsystem prompt preview: {}",
            &sys_prompt[..sys_prompt.len().min(500)]
        );
        })
        .await;
}
/// The `ECHO_AGENT_AGENT` escape hatch should override the model's agent_type.
/// Setting `ECHO_AGENT_AGENT=echo-agent-build` with an alternate-agent model should use
/// echo-agent-build harness.
#[tokio::test]
#[ignore]
async fn test_echo_agent_agent_env_overrides_model_agent_type() {
    with_local_set(|| async {
            let server = dual_model_server().await;
            let workdir = git_workdir();
            let sandbox = TestSandbox::builder().mock_url(server.url()).build();
            let client = EchoAgentStdioClient::spawn_with_sandbox_env_and_args(
                    &server,
                    workdir.workspace(),
                    sandbox,
                    &[("ECHO_AGENT_AGENT", "echo-agent-build")],
                    &[],
                )
                .await;
            client.initialize_with_timeout().await;
            let session_id = client
                .create_session_with_model_timeout(workdir.workspace(), "cursor-model")
                .await;
            let result = client.prompt_with_timeout(&session_id, "say hello").await;
            assert!(
            result.is_ok(),
            "prompt with ECHO_AGENT_AGENT override failed: {:?}\nstderr:\n{}",
            result.err(),
            client.stderr()
        );
            let sys_prompt = server
                .last_system_prompt()
                .expect("should have inference request");
            assert!(
            sys_prompt.contains("EchoAgent") || sys_prompt.contains("echoagent"),
            "ECHO_AGENT_AGENT=echo-agent-build should override catalog model agent_type\nsystem prompt preview: {}",
            &sys_prompt[..sys_prompt.len().min(500)]
        );
        })
        .await;
}
