//! Non-blocking startup regression tests. `#[ignore]`; requires pre-built binary.
//!
//! These exercise leader startup through the persistent-leader fixture: the
//! leader must bind its socket and become ready without blocking on the remote
//! `/settings` + `/v1/models` fetch, and must self-heal its catalog once the
//! endpoint recovers.
//!
//! ```bash
//! cargo test -p echo-agent-runtime --test test_nonblocking_startup -- --ignored
//! ```

#![cfg(unix)]

mod common;

use std::time::{Duration, Instant};

use echo_agent_test_support::leader::LeaderFixture;
use echo_agent_test_support::*;

async fn poll_until(ceiling: Duration, interval: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + ceiling;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// A hanging proxy must not delay leader readiness: the fixture (which waits for
/// the leader socket) must come up and a session must be created well within the
/// blocking-fetch window.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn leader_ready_while_proxy_hangs() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            server.set_hang(true);

            let workdir = git_workdir();
            let sandbox = TestSandbox::new();

            let started = Instant::now();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("leader must become ready while the proxy hangs");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn leader client"),
                    );
                    clients[0].initialize().await;
                    let _session = clients[0].create_session(workdir.workspace()).await;

                    let elapsed = started.elapsed();
                    assert!(
                        elapsed < Duration::from_secs(25),
                        "startup took {elapsed:?} with a hanging proxy; readiness appears \
                         to block on the network fetch\nstderr:\n{}",
                        clients[0].stderr_text(),
                    );
                })
            })
            .await;
        })
        .await;
}

/// The background catalog refresh must re-fetch and push `echo.agent/models/update`
/// once a previously-hanging endpoint recovers.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn catalog_self_heals_after_endpoint_recovers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            server.set_hang(true);

            let workdir = git_workdir();
            let sandbox = TestSandbox::new();

            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("leader must become ready while the proxy hangs");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn leader client"),
                    );
                    clients[0].initialize().await;

                    // Recover the endpoint. The background catalog refresh
                    // (5s-base backoff) re-fetches and pushes `echo.agent/models/update`.
                    server.set_hang(false);

                    let healed =
                        poll_until(Duration::from_secs(60), Duration::from_millis(500), || {
                            clients[0].models_update_count() > 0
                        })
                        .await;
                    assert!(
                        healed,
                        "no echo.agent/models/update after the endpoint recovered\nstderr:\n{}",
                        clients[0].stderr_text(),
                    );
                })
            })
            .await;
        })
        .await;
}

/// Custom-backend reality: a user points at their own backend that serves
/// `/v1/models` + chat but blocks the cli-chat-proxy `/settings` (404). The
/// leader must boot fast, load its catalog from the served models, and run a
/// real prompt, even though remote settings never arrive.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn leader_usable_when_settings_blocked_but_models_served() {
    tokio::task::LocalSet::new()
        .run_until(async {
            // Models + chat are served; `set_settings` is never called, so
            // `/v1/settings` 404s, mimicking a blocked proxy settings endpoint.
            let server = MockInferenceServer::start().await.unwrap();

            let workdir = git_workdir();
            let sandbox = TestSandbox::new();

            let started = Instant::now();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("leader must become ready with /settings blocked");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn leader client"),
                    );
                    clients[0].initialize().await;
                    let session = clients[0].create_session(workdir.workspace()).await;

                    // The custom backend serves chat, so a prompt round-trips
                    // despite the blocked settings endpoint.
                    clients[0]
                        .prompt(&session, "ping")
                        .await
                        .expect("prompt against the custom backend");
                    assert!(
                        server.has_chat_completion_request() || server.has_responses_request(),
                        "the prompt must reach the served chat endpoint\nstderr:\n{}",
                        clients[0].stderr_text(),
                    );

                    assert!(
                        started.elapsed() < Duration::from_secs(25),
                        "startup/usage blocked on the unreachable /settings\nstderr:\n{}",
                        clients[0].stderr_text(),
                    );
                    assert_eq!(
                        clients[0].settings_update_count(),
                        0,
                        "no settings update should land while /settings is blocked",
                    );
                })
            })
            .await;
        })
        .await;
}
