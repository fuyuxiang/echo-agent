//! Scenario tests driving [`PagerLeaderCluster`] (the harness lives in
//! `mod.rs`). New multi-client scenarios land here so the harness and its
//! consumers grow independently.

use super::*;

const T1: &str = "CLUSTER_SENTINEL_T1";
const T2: &str = "CLUSTER_SENTINEL_T2";

/// Two clients share one session: the driver's turn streams into the
/// attached viewer live, replay renders exactly once, and the
/// driver/viewer roles flip for the next turn. In-process port of the
/// `leader_two_clients_shared_session` PTY case.
#[test]
#[ignore = "leader-cluster: needs single-process isolation (process-global env + echo_agent_home OnceLock in the shared lib test binary); run: cargo test -p echo-agent-pager --lib -- app::leader_cluster --ignored --test-threads=1"]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn two_clients_share_session_and_stream_both_ways() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let mut cluster = PagerLeaderCluster::start().await;
        let mut a = cluster.client("cluster-a", false).await;

        cluster.server.set_response(format!("{T1} first turn."));
        let sid = a.new_session().await;
        a.run_turn("go", T1).await;

        // Viewer attaches and replays the transcript exactly once.
        let mut b = cluster.client("cluster-b", false).await;
        b.load_session(&sid).await;
        b.pump_until("replay reaches viewer", move |app| {
            app.agents
                .values()
                .any(|agent| agent_message_text(agent).contains(T1))
        })
        .await;
        assert_eq!(
            occurrences(&agent_message_text(b.agent_for_session(&sid)), T1),
            1,
            "replayed turn must render exactly once in the viewer"
        );

        // Turn 2 driven from the driver streams into the viewer live.
        cluster.server.set_response(format!("{T2} second turn."));
        a.act(Action::SendPrompt("again".to_string()));
        pump_clients_until(&mut [&mut a, &mut b], "turn 2 fans out", |clients| {
            clients.iter().all(|c| {
                c.app
                    .agents
                    .values()
                    .any(|agent| agent_message_text(agent).contains(T2))
            })
        })
        .await;
        assert_eq!(
            occurrences(&agent_message_text(b.agent_for_session(&sid)), T2),
            1,
            "live turn must render exactly once in the viewer"
        );
        assert_eq!(
            occurrences(&agent_message_text(a.agent_for_session(&sid)), T2),
            1,
            "live turn must render exactly once in the driver"
        );

        a.sever();
        b.sever();
    });
}

/// N-client fan-out: one driver, three viewers. Live turns broadcast to
/// every subscriber exactly once; each viewer's attach replay is unicast
/// (it never duplicates into the already-attached clients).
#[test]
#[ignore = "leader-cluster: needs single-process isolation (process-global env + echo_agent_home OnceLock in the shared lib test binary); run: cargo test -p echo-agent-pager --lib -- app::leader_cluster --ignored --test-threads=1"]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn n_client_fan_out_without_replay_duplication() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let mut cluster = PagerLeaderCluster::start().await;
        let mut driver = cluster.client("cluster-driver", false).await;

        cluster.server.set_response(format!("{T1} fan-out seed."));
        let sid = driver.new_session().await;
        driver.run_turn("go", T1).await;

        let mut viewers = Vec::new();
        for i in 0..3 {
            let mut viewer = cluster.client(&format!("cluster-viewer-{i}"), false).await;
            viewer.load_session(&sid).await;
            viewer
                .pump_until("viewer replay lands", move |app| {
                    app.agents
                        .values()
                        .any(|agent| agent_message_text(agent).contains(T1))
                })
                .await;
            assert_eq!(
                occurrences(&agent_message_text(viewer.agent_for_session(&sid)), T1),
                1,
                "viewer {i} must replay the transcript exactly once"
            );
            viewers.push(viewer);
        }
        // The driver must NOT have received any copy of the unicast replays.
        driver.pump_once();
        assert_eq!(
            occurrences(&agent_message_text(driver.agent_for_session(&sid)), T1),
            1,
            "attach replays must be unicast — never duplicated into the driver"
        );

        // A live turn reaches all four clients exactly once each.
        cluster.server.set_response(format!("{T2} fan-out live."));
        driver.act(Action::SendPrompt("again".to_string()));
        let mut all: Vec<&mut ClusterClient> = Vec::new();
        all.push(&mut driver);
        for viewer in viewers.iter_mut() {
            all.push(viewer);
        }
        pump_clients_until(&mut all, "live turn fans out to all", |clients| {
            clients.iter().all(|c| {
                c.app
                    .agents
                    .values()
                    .any(|agent| agent_message_text(agent).contains(T2))
            })
        })
        .await;
        for (i, client) in all.iter().enumerate() {
            assert_eq!(
                occurrences(&agent_message_text(client.agent_for_session(&sid)), T2),
                1,
                "client {i} must see the live turn exactly once"
            );
        }
        // Release the &mut borrows before consuming the clients.
        drop(all);

        for viewer in viewers {
            viewer.sever();
        }
        driver.sever();
    });
}

/// Reattach after the driver disconnects: a completed turn round-trips
/// through the durable log — the fresh client replays it exactly once,
/// lands Idle, and no inference request is re-driven. In-process port of
/// `leader_reattach_completion_roundtrips_durable_log`.
#[test]
#[ignore = "leader-cluster: needs single-process isolation (process-global env + echo_agent_home OnceLock in the shared lib test binary); run: cargo test -p echo-agent-pager --lib -- app::leader_cluster --ignored --test-threads=1"]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn reattach_completion_roundtrips_durable_log() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let mut cluster = PagerLeaderCluster::start().await;
        let mut a = cluster.client("cluster-a", false).await;

        cluster.server.set_response(format!("{T1} durable turn."));
        let sid = a.new_session().await;
        a.run_turn("go", T1).await;

        // Keep-alive viewer so the session stays resident across A's exit
        // (parity with the PTY case; the in-process server also survives).
        let mut keep = cluster.client("cluster-keepalive", false).await;
        keep.load_session(&sid).await;

        // The completed turn is durable on disk before the reattach. The
        // write is async relative to the PromptResponse, so poll briefly
        // (the PTY port does the same via wait_for_turn_completed).
        let durable_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let recorded = find_session_updates_file(&sid).is_some_and(|updates| {
                std::fs::read_to_string(&updates)
                    .unwrap_or_default()
                    .contains("turn_completed")
            });
            if recorded {
                break;
            }
            assert!(
                tokio::time::Instant::now() < durable_deadline,
                "durable log never recorded the turn terminal for {sid}"
            );
            tokio::time::sleep(PUMP_TICK).await;
        }

        let inference_before = cluster.inference_request_count();
        a.sever();

        let mut c = cluster.client("cluster-reattach", false).await;
        c.load_session(&sid).await;
        c.pump_until("reattach replay lands", move |app| {
            app.agents
                .values()
                .any(|agent| agent_message_text(agent).contains(T1))
        })
        .await;

        let view = c.agent_for_session(&sid);
        assert_eq!(
            occurrences(&agent_message_text(view), T1),
            1,
            "reattach must replay the completed transcript exactly once"
        );
        assert!(
            matches!(view.session.state, AgentState::Idle),
            "a completed turn must reattach Idle, not stuck waiting"
        );
        assert_eq!(
            cluster.inference_request_count(),
            inference_before,
            "reattach must replay from the durable log, not re-drive a turn"
        );

        keep.sever();
        c.sever();
    });
}

/// Leader kill → bridge reconnect (real `LeaderReconnector`, socket
/// pinned) → manual reload dance (`begin_session_reload` → `session/load`
/// → `finish_session_reload`) → history renders exactly once and new
/// turns work. The reload-driver mirrors the event loop's reconnect arm
/// (its `plan_reconnect_load` is event_loop-private; the cwd/meta
/// derivation is replicated inline).
#[test]
#[ignore = "leader-cluster: needs single-process isolation (process-global env + echo_agent_home OnceLock in the shared lib test binary); run: cargo test -p echo-agent-pager --lib -- app::leader_cluster --ignored --test-threads=1"]
#[serial_test::serial(ECHO_AGENT_HOME)]
fn leader_kill_reconnect_reloads_without_duplicating_history() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let mut cluster = PagerLeaderCluster::start().await;
        let mut a = cluster.client("cluster-reconnect", true).await;

        cluster.server.set_response(format!("{T1} pre-crash turn."));
        let sid = a.new_session().await;
        a.run_turn("go", T1).await;

        // Kill the leader generation, respawn at the same path; the
        // bridge's reconnector adopts the new in-process server.
        cluster.kill_leader().await;
        cluster.respawn_leader().await;

        let mut status_rx = a.status_rx.clone().expect("reconnecting client");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if matches!(
                *status_rx.borrow_and_update(),
                ConnectionStatus::Connected { generation } if generation >= 1
            ) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "bridge never reconnected to the respawned leader"
            );
            a.pump_once();
            tokio::time::sleep(PUMP_TICK).await;
        }

        // Reconnect re-init: initialize, then reload each session tab.
        let _init: acp::InitializeResponse = bounded(
            "reconnect re-initialize",
            acp_send(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
                    acp::ClientCapabilities::new()
                        .fs(acp::FileSystemCapabilities::new())
                        .terminal(false),
                ),
                &a.app.acp_tx,
            ),
        )
        .await
        .expect("re-initialize after reconnect");
        let _: acp::AuthenticateResponse = bounded(
            "reconnect re-authenticate",
            acp_send(
                acp::AuthenticateRequest::new(acp::AuthMethodId::new("echoagent.api_key"))
                    .meta(serde_json::json!({ "headless": true }).as_object().cloned()),
                &a.app.acp_tx,
            ),
        )
        .await
        .expect("re-authenticate after reconnect");
        cluster.authenticated = true;

        let (agent_id, plan) = {
            let (id, agent) = a
                .app
                .agents
                .iter()
                .find(|(_, agent)| {
                    agent
                        .session
                        .session_id
                        .as_ref()
                        .is_some_and(|s| s.0.as_ref() == sid)
                })
                .expect("reconnecting tab exists");
            // plan_reconnect_load equivalent: session id + session cwd +
            // yolo/auto/cursor meta.
            let cwd = if agent.session.cwd.as_os_str().is_empty() {
                a.app.cwd.clone()
            } else {
                agent.session.cwd.clone()
            };
            let mut meta = serde_json::json!({ "yoloMode": false, "autoMode": false });
            if let Some(ref cursor) = agent.last_seen_event_id {
                meta["cursor"] = serde_json::Value::String(cursor.clone());
            }
            (*id, (agent.session.session_id.clone().unwrap(), cwd, meta))
        };
        a.app
            .agents
            .get_mut(&agent_id)
            .unwrap()
            .begin_session_reload(1);
        let (load_sid, load_cwd, load_meta) = plan;
        let load = bounded(
            "reconnect session/load",
            acp_send(
                acp::LoadSessionRequest::new(load_sid, load_cwd)
                    .mcp_servers(vec![])
                    .meta(load_meta.as_object().cloned()),
                &a.app.acp_tx,
            ),
        )
        .await;
        let ok = load.is_ok();
        // Drain the replay the load unicast to this client BEFORE
        // finalizing, mirroring the production replay-then-finalize order.
        a.pump_until("reload replay lands", move |app| {
            app.agents
                .values()
                .any(|agent| agent_message_text(agent).contains(T1))
        })
        .await;
        a.app
            .agents
            .get_mut(&agent_id)
            .unwrap()
            .finish_session_reload(1, ok);
        assert!(ok, "reconnect session/load failed: {:?}", load.err());

        assert_eq!(
            occurrences(&agent_message_text(a.agent_for_session(&sid)), T1),
            1,
            "reconnect reload must not duplicate history"
        );

        // The reconnected generation serves new turns.
        cluster
            .server
            .set_response(format!("{T2} post-crash turn."));
        a.run_turn("after crash", T2).await;
        assert_eq!(
            occurrences(&agent_message_text(a.agent_for_session(&sid)), T2),
            1
        );

        a.sever();
    });
}

/// Locate `<echo_agent_home>/sessions/<enc-cwd>/<sid>/updates.jsonl` without the
/// (internal) cwd encoder: scan one level of cwd dirs (same approach as
/// session_load_perf's `locate_session_dir`).
fn find_session_updates_file(sid: &str) -> Option<PathBuf> {
    let sessions = effective_echo_agent_home().join("sessions");
    for entry in std::fs::read_dir(&sessions).ok()?.flatten() {
        let candidate = entry.path().join(sid).join("updates.jsonl");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
