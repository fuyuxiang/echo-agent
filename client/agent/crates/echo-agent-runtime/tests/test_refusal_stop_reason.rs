//! Regression tests: a Anthropic Messages API `/v1/messages` stream that terminates with
//! `stop_reason: "refusal"` must complete the turn cleanly with EXACTLY ONE
//! inference request.
//!
//! Previously the unknown `stop_reason` failed the terminal `message_delta`
//! parse (discarding the fully-streamed response) and the resulting
//! serialization error was misclassified as a retryable stream error,
//! producing a ~10-minute retry storm per turn. Covered here end-to-end
//! through both the plain stdio agent and a leader-hosted session.
//!
//! Tests are `#[ignore]`d by default — they require a pre-built binary
//! (auto-built locally when missing):
//!
//! ```bash
//! cargo test -p echo-agent-runtime --test test_refusal_stop_reason -- --ignored
//! ```

use std::future::Future;

#[cfg(unix)]
mod common;

use agent_client_protocol as acp;
use echo_agent_test_support::*;

/// Run an async test body inside a `LocalSet` (required by ACP's `!Send` futures).
async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

/// Mock with a single Anthropic-style model whose `/v1/messages` stream ends
/// with `stop_reason: "refusal"`.
async fn refusal_messages_server() -> MockInferenceServer {
    let server = MockInferenceServer::start_with_models(vec![
        MockModelEntry::new("messages-compatible-model").with_api_backend("messages"),
    ])
    .await
    .expect("start mock server");
    server.set_messages_stop_reason("refusal");
    server
}

/// `/v1/messages` requests belonging to the prompt turn. The session also
/// fires a one-shot title-generation call on the first user message; it is
/// identified (and excluded) by its forced `session_title` tool.
fn turn_messages_request_count(server: &MockInferenceServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|e| e.path == "/v1/messages")
        .filter(|e| {
            !e.body.as_ref().is_some_and(|b| {
                b.get("tools")
                    .and_then(|t| t.as_array())
                    .is_some_and(|tools| {
                        tools.iter().any(|t| {
                            t.get("name").and_then(|n| n.as_str()) == Some("session_title")
                        })
                    })
            })
        })
        .count()
}

/// THE regression test: a refusal-terminated `/v1/messages` turn must return
/// a successful prompt response from exactly one inference request.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_refusal_turn_completes_with_single_messages_request() {
    with_local_set(|| async {
        let server = refusal_messages_server().await;
        let workdir = git_workdir();
        let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;

        client.initialize_with_timeout().await;
        let session_id = client
            .create_session_with_model_timeout(workdir.workspace(), "messages-compatible-model")
            .await;

        let result = client.prompt_with_timeout(&session_id, "say hello").await;
        let response = result.unwrap_or_else(|e| {
            panic!(
                "refusal-terminated turn must complete, got error: {e:?}\nrequest log:\n{}\nstderr:\n{}",
                server.request_log_summary(),
                stderr_tail(&client.stderr(), 1200)
            )
        });
        assert_eq!(
            response.stop_reason,
            acp::StopReason::EndTurn,
            "refusal must end the turn cleanly"
        );
        assert!(
            client.captured_text().contains("Echo:"),
            "streamed response text must be delivered, got: {:?}",
            client.captured_text()
        );
        assert_eq!(
            turn_messages_request_count(&server),
            1,
            "exactly one turn request to /v1/messages (no retry storm)\nrequest log:\n{}",
            server.request_log_summary()
        );
        assert!(
            server.messages_request_count() <= 2,
            "at most turn + title-generation requests\nrequest log:\n{}",
            server.request_log_summary()
        );
    })
    .await;
}

// ============================================================================
// Leader mode: the same refusal scenario through a leader-hosted session
// (client → stdio bridge → leader unix socket → leader-hosted agent).
// ============================================================================

#[cfg(unix)]
mod leader {
    use std::time::Duration;

    use agent_client_protocol as acp;

    use echo_agent_test_support::leader::{LeaderFixture, wait_for_live_leader};
    use echo_agent_test_support::*;

    use super::{common, refusal_messages_server, turn_messages_request_count, with_local_set};

    /// Leader-mode variant of the regression: the refusal-terminated turn
    /// must complete cleanly (single request, prompt response delivered)
    /// when the session is hosted by the leader IPC server.
    #[tokio::test]
    #[ignore] // requires pre-built binary; run with --ignored
    async fn test_leader_refusal_turn_completes_with_single_messages_request() {
        with_local_set(|| async {
            let server = refusal_messages_server().await;
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("start persistent leader fixture");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(
                &fixture,
                &mut clients,
                |fixture, clients| {
                    Box::pin(async move {
                        clients.push(
                            fixture
                                .spawn_client(&server, workdir.workspace(), &sandbox)
                                .await
                                .expect("spawn leader client"),
                        );
                        let client = &clients[0];
                        client.initialize().await;
                        let session_id = client
                            .create_session_with_model(
                                workdir.workspace(),
                                "messages-compatible-model",
                            )
                            .await;

                        let result = client.prompt(&session_id, "say hello").await;

                        // Prove the session is leader-hosted: a live leader process,
                        // distinct from the client subprocess, holds the lock.
                        let leader_pid =
                            wait_for_live_leader(sandbox.home(), Duration::from_secs(5))
                                .await
                                .unwrap_or_else(|| {
                                    panic!(
                                        "no live leader PID in lock file — turn did not run under the leader\nstderr:\n{}",
                                        client.stderr_text()
                                    )
                                });
                        assert_ne!(
                            Some(leader_pid),
                            client.child_pid(),
                            "leader must be a separate process from the stdio client"
                        );
                        let response = result.unwrap_or_else(|e| {
                            panic!(
                                "leader-hosted refusal turn must complete, got error: {e:?}\nrequest log:\n{}\nstderr:\n{}",
                                server.request_log_summary(),
                                client.stderr_text()
                            )
                        });
                        assert_eq!(
                            response.stop_reason,
                            acp::StopReason::EndTurn,
                            "refusal must end the turn cleanly under the leader"
                        );
                        assert!(
                            client.captured_text().contains("Echo:"),
                            "streamed response text must reach the client through the leader, got: {:?}",
                            client.captured_text()
                        );
                        assert_eq!(
                            turn_messages_request_count(&server),
                            1,
                            "exactly one turn request to /v1/messages (no retry storm)\nrequest log:\n{}",
                            server.request_log_summary()
                        );
                        assert!(
                            server.messages_request_count() <= 2,
                            "at most turn + title-generation requests\nrequest log:\n{}",
                            server.request_log_summary()
                        );
                    })
                },
            )
            .await;
        })
        .await;
    }
}
