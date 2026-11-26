//! Repro + regression test: leader process dies → connected clients must
//! re-elect a leader and transparently restore their sessions.
//!
//! Scenario (mirrors the field report "clients see `unknown session id` after
//! the leader dies"):
//!
//! 1. Two stdio clients (`echoagent agent --leader stdio`) share one leader.
//! 2. Each creates its own session and completes a prompt round-trip.
//! 3. The leader is killed with SIGKILL (crash, no graceful shutdown).
//! 4. Each client's bridge must reconnect (re-electing / spawning a fresh
//!    leader), replay `initialize` + `session/load`, and then prompts against
//!    the ORIGINAL session IDs must succeed again.
//!
//! Tests are `#[ignore]`d by default — they require a pre-built binary:
//!
//! ```bash
//! cargo test -p echo-agent-runtime --test test_leader_death_repro -- --ignored --nocapture
//! ```

#![cfg(unix)]

mod common;

use std::time::Duration;

use agent_client_protocol::{self as acp, Agent as _};
use echo_agent_test_support::leader::{
    LeaderFixture, leader_log, wait_for_live_leader, wait_for_replay_notifications,
};
use echo_agent_test_support::*;

/// Kill the shared leader while two clients are connected; both must recover.
#[tokio::test]
#[ignore = "leader-acceptance: detached replacement cleanup needs OS containment or a test-only leader binary"]
async fn test_leader_sigkill_clients_recover_sessions() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("start persistent leader fixture");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn client A"),
                    );
                    clients[0].initialize().await;
                    let session_a = clients[0].create_session(workdir.workspace()).await;
                    clients[0]
                        .prompt(&session_a, "hello from A")
                        .await
                        .expect("pre-crash prompt A");

                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn client B"),
                    );
                    clients[1].initialize().await;
                    let session_b = clients[1].create_session(workdir.workspace()).await;
                    clients[1]
                        .prompt(&session_b, "hello from B")
                        .await
                        .expect("pre-crash prompt B");

                    let leader_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(5))
                        .await
                        .expect("live leader");
                    let base_a = clients[0].notification_count();
                    let base_b = clients[1].notification_count();
                    assert_eq!(
                        fixture
                            .kill_current_concrete_leader()
                            .expect("kill owned leader"),
                        leader_pid
                    );
                    fixture
                        .reap_exited_concrete_leaders()
                        .await
                        .expect("reap crashed concrete leader");
                    let _new_pid = fixture
                        .wait_for_new_leader(leader_pid, Duration::from_secs(60))
                        .await
                        .unwrap_or_else(|_| {
                            panic!(
                                "no replacement leader\nA:\n{}\nB:\n{}\nleader:\n{}",
                                clients[0].stderr_text(),
                                clients[1].stderr_text(),
                                leader_log(sandbox.home()),
                            )
                        });
                    wait_for_replay_notifications(&clients[0], base_a, Duration::from_secs(60))
                        .await;
                    wait_for_replay_notifications(&clients[1], base_b, Duration::from_secs(60))
                        .await;
                    clients[0]
                        .prompt(&session_a, "after crash A")
                        .await
                        .expect("client A recovery");
                    clients[1]
                        .prompt(&session_b, "after crash B")
                        .await
                        .expect("client B recovery");
                })
            })
            .await;
        })
        .await;
}

/// Single-client recovery variant.
#[tokio::test]
#[ignore = "leader-acceptance: detached replacement cleanup needs OS containment or a test-only leader binary"]
async fn test_leader_sigkill_single_client_recovers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("start fixture");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn client"),
                    );
                    clients[0].initialize().await;
                    let session = clients[0].create_session(workdir.workspace()).await;
                    clients[0].prompt(&session, "hello").await.expect("prompt");
                    let leader_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(5))
                        .await
                        .expect("live leader");
                    let base = clients[0].notification_count();
                    assert_eq!(
                        fixture
                            .kill_current_concrete_leader()
                            .expect("kill owned leader"),
                        leader_pid
                    );
                    let _new_pid = fixture
                        .wait_for_new_leader(leader_pid, Duration::from_secs(60))
                        .await
                        .expect("replacement leader");
                    wait_for_replay_notifications(&clients[0], base, Duration::from_secs(60)).await;
                    clients[0]
                        .prompt(&session, "after crash")
                        .await
                        .expect("recovered prompt");
                })
            })
            .await;
        })
        .await;
}

/// One client must restore both of its sessions after re-election.
#[tokio::test]
#[ignore = "leader-acceptance: detached replacement cleanup needs OS containment or a test-only leader binary"]
async fn test_leader_sigkill_multi_session_client_recovers_all_sessions() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("start fixture");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn client"),
                    );
                    clients[0].initialize().await;
                    let session_one = clients[0].create_session(workdir.workspace()).await;
                    clients[0]
                        .prompt(&session_one, "hello one")
                        .await
                        .expect("session one prompt");
                    let session_two = clients[0].create_session(workdir.workspace()).await;
                    clients[0]
                        .prompt(&session_two, "hello two")
                        .await
                        .expect("session two prompt");
                    let leader_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(5))
                        .await
                        .expect("live leader");
                    let base = clients[0].notification_count();
                    assert_eq!(
                        fixture
                            .kill_current_concrete_leader()
                            .expect("kill owned leader"),
                        leader_pid
                    );
                    let _new_pid = fixture
                        .wait_for_new_leader(leader_pid, Duration::from_secs(60))
                        .await
                        .expect("replacement leader");
                    wait_for_replay_notifications(&clients[0], base, Duration::from_secs(60)).await;
                    clients[0]
                        .prompt(&session_one, "after crash one")
                        .await
                        .expect("session one recovery");
                    clients[0]
                        .prompt(&session_two, "after crash two")
                        .await
                        .expect("session two recovery");
                })
            })
            .await;
        })
        .await;
}

/// A prompt queued during re-election must be delivered after recovery.
#[tokio::test]
#[ignore = "leader-acceptance: detached replacement cleanup needs OS containment or a test-only leader binary"]
async fn test_prompt_sent_during_outage_is_delivered_after_recovery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture = LeaderFixture::start(&server, workdir.workspace(), &sandbox)
                .await
                .expect("start fixture");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client(&server, workdir.workspace(), &sandbox)
                            .await
                            .expect("spawn client"),
                    );
                    clients[0].initialize().await;
                    let session = clients[0].create_session(workdir.workspace()).await;
                    clients[0].prompt(&session, "hello").await.expect("prompt");
                    let leader_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(5))
                        .await
                        .expect("live leader");
                    assert_eq!(
                        fixture
                            .kill_current_concrete_leader()
                            .expect("kill owned leader"),
                        leader_pid
                    );
                    tokio::time::sleep(Duration::from_millis(300)).await;

                    tokio::time::timeout(
                        Duration::from_secs(90),
                        clients[0].conn.prompt(acp::PromptRequest::new(
                            session.clone(),
                            vec![acp::ContentBlock::Text(acp::TextContent::new(
                                "sent during outage".to_string(),
                            ))],
                        )),
                    )
                    .await
                    .expect("outage prompt timeout")
                    .expect("outage prompt failed");
                    let _new_pid = fixture
                        .wait_for_new_leader(leader_pid, Duration::from_secs(60))
                        .await
                        .expect("replacement leader");
                    clients[0]
                        .conn
                        .set_session_model(acp::SetSessionModelRequest::new(
                            session,
                            acp::ModelId::new("test-model"),
                        ))
                        .await
                        .expect("set model after recovery");
                })
            })
            .await;
        })
        .await;
}
