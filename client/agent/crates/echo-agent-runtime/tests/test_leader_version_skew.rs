//! Two-binary version-skew tests: a real OLD released binary and a real NEW
//! binary sharing one leader socket. This is the only harness that exercises
//! cross-version eviction with real processes.
//!
//! Binaries are resolved per role:
//! - `ECHO_AGENT_BINARY_LEADER` — the binary that elects the initial leader.
//! - `ECHO_AGENT_BINARY_CLIENT` — the second client.
//!
//! These ignored tests require two pre-built binaries.

#![cfg(unix)]

mod common;

use std::path::Path;
use std::time::Duration;

use echo_agent_runtime::leader::{
    ClientCapabilities, ClientMode, ControlCommand, ControlPayload, LeaderClient,
};
use echo_agent_test_support::leader::{
    LeaderFixture, client_binary, leader_binary, leader_log, pid_alive, read_leader_pid,
    wait_for_live_leader, wait_for_replay_notifications,
};
use echo_agent_test_support::*;

/// Skip when both roles resolve to the same binary; such a run tests no skew.
fn skew_binaries() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let old = leader_binary();
    let new = client_binary();
    if old == new {
        eprintln!(
            "SKIP: ECHO_AGENT_BINARY_LEADER/ECHO_AGENT_BINARY_CLIENT resolve to the same binary ({})",
            old.display()
        );
        return None;
    }
    Some((old, new))
}

async fn wait_for_pid_death(pid: u32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn sandbox_unified_log(home: &Path) -> String {
    std::fs::read_to_string(home.join(".echo-agent").join("logs").join("unified.jsonl"))
        .unwrap_or_default()
}

/// A new client evicts an old leader and the old session survives replay.
#[tokio::test]
#[ignore = "leader-acceptance: version-skew replacement cleanup needs OS containment or a test-only leader binary"]
async fn new_client_evicts_old_leader_and_sessions_reload() {
    let Some((old_bin, new_bin)) = skew_binaries() else {
        return;
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture =
                LeaderFixture::start_with_binary(&old_bin, &server, workdir.workspace(), &sandbox)
                    .await
                    .expect("start owned version-skew leader");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(
                &fixture,
                &mut clients,
                |fixture, clients| {
                    Box::pin(async move {
                        clients.push(
                            fixture
                                .spawn_client_with_binary(
                                    &old_bin,
                                    &server,
                                    workdir.workspace(),
                                    &sandbox,
                                )
                                .await
                                .expect("spawn old leader client"),
                        );
                        clients[0].initialize().await;
                        let session = clients[0].create_session(workdir.workspace()).await;
                        clients[0]
                            .prompt(&session, "hello from the old world")
                            .await
                            .expect("pre-skew prompt failed");
                        let old_pid =
                            wait_for_live_leader(sandbox.home(), Duration::from_secs(10))
                                .await
                                .expect("no live old leader");
                        let base = clients[0].notification_count();

                        clients.push(
                            fixture
                                .spawn_client_with_binary(
                                    &new_bin,
                                    &server,
                                    workdir.workspace(),
                                    &sandbox,
                                )
                                .await
                                .expect("spawn new leader client"),
                        );
                        clients[1].initialize().await;
                        let _new_pid = fixture
                            .wait_for_new_leader(old_pid, Duration::from_secs(60))
                            .await
                            .unwrap_or_else(|_| {
                                panic!(
                                    "no replacement leader after version-floor eviction\nold stderr:\n{}\nnew stderr:\n{}\nleader log:\n{}",
                                    clients[0].stderr_text(),
                                    clients[1].stderr_text(),
                                    leader_log(sandbox.home()),
                                )
                            });
                        assert_ne!(_new_pid, old_pid);
                        assert!(
                            wait_for_pid_death(old_pid, Duration::from_secs(30)).await,
                            "old leader pid {old_pid} still alive after eviction\nleader log:\n{}",
                            leader_log(sandbox.home()),
                        );

                        wait_for_replay_notifications(
                            &clients[0],
                            base,
                            Duration::from_secs(60),
                        )
                        .await;
                        let response = clients[0].prompt(&session, "after the eviction").await;
                        assert!(
                            response.is_ok(),
                            "old client prompt after eviction failed: {:?}\nstderr:\n{}\nleader log:\n{}",
                            response.err(),
                            clients[0].stderr_text(),
                            leader_log(sandbox.home()),
                        );
                        let new_session = clients[1].create_session(workdir.workspace()).await;
                        clients[1]
                            .prompt(&new_session, "hello from the new world")
                            .await
                            .expect("new client prompt failed");
                    })
                },
            )
            .await;
        })
        .await;
}

/// An old client adopts a directly-owned new leader without triggering a downgrade.
#[tokio::test]
#[ignore = "two-binary version-skew test; set ECHO_AGENT_BINARY_LEADER/ECHO_AGENT_BINARY_CLIENT and run with --ignored"]
async fn old_client_adopts_new_leader_and_still_functions() {
    let Some((old_bin, new_bin)) = skew_binaries() else {
        return;
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture =
                LeaderFixture::start_with_binary(&new_bin, &server, workdir.workspace(), &sandbox)
                    .await
                    .expect("start owned new leader");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &new_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn new leader client"),
                    );
                    clients[0].initialize().await;
                    let leader_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(10))
                        .await
                        .expect("no live new leader");

                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &old_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn old leader client"),
                    );
                    clients[1].initialize().await;
                    assert_eq!(
                        read_leader_pid(sandbox.home()),
                        Some(leader_pid),
                        "an older client must never evict a newer leader"
                    );
                    let session = clients[1].create_session(workdir.workspace()).await;
                    clients[1]
                        .prompt(&session, "old client on new leader")
                        .await
                        .expect("old client prompt on new leader failed");

                    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    let mut saw_mismatch = false;
                    while tokio::time::Instant::now() < deadline {
                        if leader_log(sandbox.home()).contains("Version mismatch") {
                            saw_mismatch = true;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    assert!(
                        saw_mismatch,
                        "leader never logged the version mismatch\nleader log:\n{}",
                        leader_log(sandbox.home()),
                    );
                })
            })
            .await;
        })
        .await;
}

/// Update relaunch exits the current leader and elects another current binary.
#[tokio::test]
#[ignore = "leader-acceptance: version-skew replacement cleanup needs OS containment or a test-only leader binary"]
async fn relaunch_for_update_drives_real_old_leader_to_exit() {
    let Some((old_bin, new_bin)) = skew_binaries() else {
        return;
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture =
                LeaderFixture::start_with_binary(&old_bin, &server, workdir.workspace(), &sandbox)
                    .await
                    .expect("start owned version-skew leader");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &old_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn old leader client"),
                    );
                    clients[0].initialize().await;
                    let session = clients[0].create_session(workdir.workspace()).await;
                    clients[0]
                        .prompt(&session, "before relaunch")
                        .await
                        .expect("pre-relaunch prompt failed");
                    let old_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(10))
                        .await
                        .expect("no live old leader");

                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &new_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn new leader client"),
                    );
                    clients[1].initialize().await;
                    let current_pid = fixture
                        .wait_for_new_leader(old_pid, Duration::from_secs(60))
                        .await
                        .expect("current client must replace old leader");
                    clients[0]
                        .close()
                        .await
                        .expect("close old client before relaunch");

                    let control = LeaderClient::connect(
                        sandbox.home().join(".echo-agent").join("leader.sock"),
                        "echo-agent-pager-update",
                        ClientMode::Stdio,
                        ClientCapabilities::default(),
                    )
                    .await
                    .expect("control connect to current leader failed");
                    if !control.registration().supports_relaunch() {
                        eprintln!(
                            "SKIP: current leader {:?} does not advertise relaunch_v1",
                            control.registration().leader_binary_version
                        );
                        control.cancel();
                        return;
                    }

                    let ack = control
                        .send_control(ControlCommand::RelaunchForUpdate {
                            to_version: "999.0.0".to_string(),
                        })
                        .await;
                    control.cancel();
                    match ack {
                        Ok(Ok(ControlPayload::Relaunching { .. })) | Err(_) => {}
                        other => panic!("unexpected RelaunchForUpdate reply: {other:?}"),
                    }
                    assert!(
                        wait_for_pid_death(current_pid, Duration::from_secs(30)).await,
                        "leader pid {current_pid} did not exit after relaunch\nleader log:\n{}",
                        leader_log(sandbox.home()),
                    );
                    let _new_pid = fixture
                        .wait_for_new_leader(current_pid, Duration::from_secs(60))
                        .await
                        .expect("no re-elected leader after relaunch");
                })
            })
            .await;
        })
        .await;
}

/// Eviction leaves one leader and does not race auth-file ownership.
#[tokio::test]
#[ignore = "leader-acceptance: version-skew replacement cleanup needs OS containment or a test-only leader binary"]
async fn eviction_leaves_single_leader_and_single_auth_owner() {
    let Some((old_bin, new_bin)) = skew_binaries() else {
        return;
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let workdir = git_workdir();
            let sandbox = TestSandbox::new();
            let fixture =
                LeaderFixture::start_with_binary(&old_bin, &server, workdir.workspace(), &sandbox)
                    .await
                    .expect("start owned version-skew leader");
            let mut clients = Vec::new();
            common::leader::run_with_cleanup(&fixture, &mut clients, |fixture, clients| {
                Box::pin(async move {
                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &old_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn old leader client"),
                    );
                    clients[0].initialize().await;
                    let old_pid = wait_for_live_leader(sandbox.home(), Duration::from_secs(10))
                        .await
                        .expect("no live old leader");
                    let auth_path = sandbox.home().join(".echo-agent").join("auth.json");
                    let auth_before = std::fs::metadata(&auth_path)
                        .ok()
                        .and_then(|metadata| metadata.modified().ok());

                    clients.push(
                        fixture
                            .spawn_client_with_binary(
                                &new_bin,
                                &server,
                                workdir.workspace(),
                                &sandbox,
                            )
                            .await
                            .expect("spawn new leader client"),
                    );
                    clients[1].initialize().await;
                    let _new_pid = fixture
                        .wait_for_new_leader(old_pid, Duration::from_secs(60))
                        .await
                        .expect("no replacement leader after eviction");
                    assert!(
                        wait_for_pid_death(old_pid, Duration::from_secs(30)).await,
                        "evicted leader must exit"
                    );
                    assert!(pid_alive(_new_pid), "replacement leader must stay alive");
                    assert_eq!(read_leader_pid(sandbox.home()), Some(_new_pid));

                    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    let mut attributed = false;
                    while tokio::time::Instant::now() < deadline {
                        let log = sandbox_unified_log(sandbox.home());
                        if log.contains("leader.evict.vacate_requested")
                            || log.contains("leader.spawn.replacement")
                        {
                            attributed = true;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    assert!(
                        attributed,
                        "eviction must be attributable in unified.jsonl\nlog:\n{}",
                        sandbox_unified_log(sandbox.home()),
                    );
                    let auth_after = std::fs::metadata(&auth_path)
                        .ok()
                        .and_then(|metadata| metadata.modified().ok());
                    assert_eq!(
                        auth_before, auth_after,
                        "auth.json must not be written during an eviction swap"
                    );
                })
            })
            .await;
        })
        .await;
}
