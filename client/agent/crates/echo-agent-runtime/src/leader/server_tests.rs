use std::time::Duration;

use super::*;
use tempfile::TempDir;

/// Parse a raw payload for the parse-once helper APIs. Panics on invalid
/// JSON — the routing loop parses once up front, and non-JSON payloads
/// never reach the helpers (they forward/drop verbatim).
fn pv(payload: &str) -> serde_json::Value {
    serde_json::from_str(payload).expect("test payload must be valid JSON")
}

/// The relaunch drain must wait on the agent-derived activity signal —
/// not just the IPC `agent_busy` flag, which relay-driven turns never set
/// — and must flush registered session actors before cancelling.
#[tokio::test]
async fn relaunch_drain_waits_for_agent_activity_and_flushes_sessions() {
    let (shutdown_tx, _shutdown_rx) =
        watch::channel(super::super::protocol::ShutdownReason::Manual);
    let cancel = CancellationToken::new();
    let agent_busy = Arc::new(AtomicBool::new(false)); // IPC view: idle
    let activity = AgentActivity::default();
    let (mut cmd_rx, prompt_id, _pending) = activity.register_for_test("s1");

    // Agent view: a relay-driven turn is running.
    *prompt_id.lock().unwrap() = Some("prompt-1".to_string());

    // Simulated session actor: exits on Shutdown, asserting cancel order.
    let cancel_for_actor = cancel.clone();
    let actor = tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if matches!(cmd, crate::session::SessionCommand::Shutdown(_)) {
                assert!(
                    !cancel_for_actor.is_cancelled(),
                    "flush must run before the leader cancels"
                );
                return;
            }
        }
    });

    spawn_relaunch_drain(shutdown_tx, cancel.clone(), agent_busy, activity);

    // Drain must hold while the turn is running.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !cancel.is_cancelled(),
        "drain must not cancel while a relay-driven turn is running"
    );

    // Turn ends → drain flushes the session, then cancels.
    *prompt_id.lock().unwrap() = None;
    tokio::time::timeout(Duration::from_secs(5), cancel.cancelled())
        .await
        .expect("drain should cancel once the agent goes idle");
    actor.await.expect("session actor should get Shutdown");
}

/// `ServerMessageRef::Acp` (the borrowed serialize-only mirror the client
/// writer uses for shared payloads) must stay byte-identical on the wire
/// to `ServerMessage::Acp`, or clients would fail to decode ACP frames.
#[test]
fn server_message_ref_is_wire_identical() {
    let payload = r#"{"jsonrpc":"2.0","method":"session/update","params":{"x":1}}"#;
    let owned = serde_json::to_vec(&ServerMessage::Acp {
        payload: payload.to_string(),
    })
    .unwrap();
    let borrowed = serde_json::to_vec(&ServerMessageRef::Acp { payload }).unwrap();
    assert_eq!(owned, borrowed);

    // And the client-side decode of the borrowed form round-trips.
    let decoded: ServerMessage = serde_json::from_slice(&borrowed).unwrap();
    match decoded {
        ServerMessage::Acp { payload: p } => assert_eq!(p, payload),
        other => panic!("expected Acp, got {other:?}"),
    }
}

/// An UNMUTATED payload forwards to the agent byte-for-byte: parsing for
/// classification must never normalize key order or whitespace of
/// pass-through traffic.
#[test]
fn outbound_payload_verbatim_when_unmutated() {
    let original = r#"{ "b" : 1,    "a": 2 }"#.to_string();
    let json = pv(&original);
    let out = select_outbound_payload(Some(&json), false, original.clone());
    assert_eq!(
        out, original,
        "unmutated payloads must forward verbatim (exact bytes, not re-serialized)"
    );
}

/// A MUTATED payload is re-serialized from the injected/rewritten `Value`
/// (semantically equal, but no longer the original odd formatting).
#[test]
fn outbound_payload_reserialized_when_mutated() {
    let original = r#"{ "b" : 1,    "a": 2 }"#.to_string();
    let json = pv(&original);
    let out = select_outbound_payload(Some(&json), true, original.clone());
    assert_ne!(
        out, original,
        "mutated payloads must be re-serialized from the Value, not the stale original"
    );
    assert_eq!(
        pv(&out),
        json,
        "the re-serialized payload must be semantically identical to the mutated Value"
    );
}

/// A non-JSON payload (`json = None`) is never parsed or re-serialized —
/// it passes through untouched, matching the old per-helper parse-failure
/// behavior.
#[test]
fn outbound_payload_non_json_passthrough() {
    let original = "not json".to_string();
    let out = select_outbound_payload(None, false, original.clone());
    assert_eq!(
        out, original,
        "non-JSON payloads must pass through verbatim"
    );
}

#[test]
fn decide_relaunch_is_idempotent_and_directional() {
    let temp = TempDir::new().unwrap();
    let sock = temp.path().join("leader.sock");
    let control_state = LeaderServerControlState::new(LeaderServerMetadata {
        pid: std::process::id(),
        socket_path: sock.clone(),
        lock_path: sock.with_extension("lock"),
        ws_url_suffix: String::new(),
        leader_binary_version: "0.1.100".to_string(),
    });
    let relaunching = AtomicBool::new(false);

    // Equal → declined, and the flag is NOT armed.
    assert!(matches!(
        decide_relaunch_for_update(&control_state, "0.1.100".to_string(), &relaunching),
        Ok(ControlPayload::RelaunchDeclined { .. })
    ));
    assert!(!relaunching.load(Ordering::SeqCst));

    // Strictly-older target (downgrade) → declined; never downgrade.
    assert!(matches!(
        decide_relaunch_for_update(&control_state, "0.1.0".to_string(), &relaunching),
        Ok(ControlPayload::RelaunchDeclined { .. })
    ));
    // Unparseable target → declined (dev "unknown" builds).
    assert!(matches!(
        decide_relaunch_for_update(&control_state, "unknown".to_string(), &relaunching),
        Ok(ControlPayload::RelaunchDeclined { .. })
    ));
    assert!(!relaunching.load(Ordering::SeqCst));

    // Newer → accepted, arms the flag.
    assert!(matches!(
        decide_relaunch_for_update(&control_state, "0.2.0".to_string(), &relaunching),
        Ok(ControlPayload::Relaunching { .. })
    ));
    assert!(relaunching.load(Ordering::SeqCst));

    // Second accepted request while armed → declined (idempotent).
    assert!(matches!(
        decide_relaunch_for_update(&control_state, "0.3.0".to_string(), &relaunching),
        Ok(ControlPayload::RelaunchDeclined { .. })
    ));
}

#[derive(Debug)]
struct TestAuth;
impl AuthProvider for TestAuth {
    fn current(&self) -> AuthCredential {
        AuthCredential::bearer("test-token")
    }
}

#[tokio::test]
async fn wait_for_leader_auth_returns_when_already_wired() {
    let ws = WorkspaceControl::new(None);
    ws.auth.send_replace(Some(Arc::new(TestAuth)));
    let cancel = CancellationToken::new();
    let auth = wait_for_leader_auth(&ws, &cancel).await.expect("wired");
    assert!(matches!(auth.current(), AuthCredential::Bearer { .. }));
}

#[tokio::test]
async fn wait_for_leader_auth_resolves_when_wired_late() {
    // A command can arrive before auth is wired; it must wait, not fail.
    let ws = Arc::new(WorkspaceControl::new(None));
    let cancel = CancellationToken::new();
    let waiter = {
        let ws = ws.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { wait_for_leader_auth(&ws, &cancel).await.is_ok() })
    };
    tokio::task::yield_now().await;
    ws.auth.send_replace(Some(Arc::new(TestAuth)));
    assert!(waiter.await.unwrap(), "auth wired late should resolve Ok");
}

#[tokio::test]
async fn workspace_start_errors_when_cancelled_before_auth() {
    // Cancel while auth is unwired → clean error, no hang.
    let state = default_test_control_state(Path::new("/tmp/echo-agent-ws-auth-test.sock"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = handle_workspace_start(state, None, "/tmp".to_string(), cancel)
        .await
        .unwrap_err();
    assert!(
        err.message.contains("shutting down"),
        "unexpected error: {}",
        err.message
    );
}

async fn setup_test_server(
    temp: &TempDir,
) -> (PathBuf, CancellationToken, mpsc::UnboundedReceiver<String>) {
    let sock_path = temp.path().join("test.sock");
    let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    (sock_path, handle.cancel, handle.acp_rx)
}

async fn setup_test_server_with_client_count(
    temp: &TempDir,
) -> (
    PathBuf,
    CancellationToken,
    mpsc::UnboundedReceiver<String>,
    Arc<AtomicUsize>,
) {
    let sock_path = temp.path().join("test.sock");
    let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    (sock_path, handle.cancel, handle.acp_rx, handle.client_count)
}

/// Like `setup_test_server` but uses `no_exit_on_disconnect=true` and
/// exposes `response_tx` for injecting agent responses.
async fn setup_persistent_server(
    temp: &TempDir,
) -> (PathBuf, CancellationToken, mpsc::UnboundedSender<String>) {
    let (sock_path, cancel, response_tx, _acp_rx) = setup_persistent_server_with_agent(temp).await;
    (sock_path, cancel, response_tx)
}

/// Like `setup_persistent_server` but also returns the agent-side receiver
/// (`acp_rx`) so a test can observe forwarded requests — e.g. to read a
/// `session/load`'s namespaced id and echo a matching load response, which
/// is required to complete a load now that live broadcasts to a loading
/// client are buffered until its load response (see `complete_load`).
async fn setup_persistent_server_with_agent(
    temp: &TempDir,
) -> (
    PathBuf,
    CancellationToken,
    mpsc::UnboundedSender<String>,
    mpsc::UnboundedReceiver<String>,
) {
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, acp_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = cancel.clone();
    let (_ready_tx, ready_rx) = watch::channel(true);
    let (shutdown_tx, _shutdown_rx) =
        watch::channel(super::super::protocol::ShutdownReason::Manual);
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            ready_rx,
            watch::channel(false).0,
            shutdown_tx,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (sock_path, cancel, response_tx, acp_rx)
}

/// Complete an in-flight `session/load` in a test: read the forwarded load
/// request from the agent channel to learn its leader-assigned namespaced
/// id, then echo a `LoadSessionResponse` with that id. This routes the
/// response back to the loading client AND flushes any live notifications
/// the leader buffered during the load window (live-before-replay guard).
async fn complete_load(
    acp_rx: &mut mpsc::UnboundedReceiver<String>,
    response_tx: &mpsc::UnboundedSender<String>,
) {
    loop {
        let forwarded = tokio::time::timeout(Duration::from_secs(1), acp_rx.recv())
            .await
            .expect("timed out waiting for forwarded session/load")
            .expect("agent channel closed");
        let json: serde_json::Value = serde_json::from_str(&forwarded).unwrap();
        if json.get("method").and_then(|m| m.as_str()) == Some("session/load") {
            let id = json.get("id").cloned().unwrap();
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "models": [] },
            });
            response_tx.send(response.to_string()).unwrap();
            return;
        }
    }
}

/// Helper to connect and register a client, returning the split stream.
async fn connect_and_register(
    sock_path: &std::path::Path,
    client_type: &str,
) -> (
    tokio::io::ReadHalf<LeaderStream>,
    tokio::io::WriteHalf<LeaderStream>,
) {
    connect_and_register_with_mode(sock_path, client_type, ClientMode::Stdio).await
}

/// Like [`connect_and_register`] but with an explicit [`ClientMode`], for
/// tests that exercise mode-dependent server behavior (relay demand).
async fn connect_and_register_with_mode(
    sock_path: &std::path::Path,
    client_type: &str,
    mode: ClientMode,
) -> (
    tokio::io::ReadHalf<LeaderStream>,
    tokio::io::WriteHalf<LeaderStream>,
) {
    let stream = LeaderStream::connect(sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: client_type.into(),
            mode,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();
    (reader, writer)
}

/// Relay demand gate (relay-on-demand): Stdio registrations must NOT
/// signal relay demand — a leader serving only interactive clients (TUI
/// dashboard, IDE) keeps the cloud.echo-agent.invalid relay off. The first Headless
/// registration (devbox / `echoagent agent headless` flow) flips the watch so
/// `run_leader` starts the deferred relay connection.
#[tokio::test]
async fn relay_demand_signals_only_on_headless_registration() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("relay-demand.sock");
    let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    let mut relay_demand_rx = handle.relay_demand_rx.clone();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A Stdio (interactive) client registers: demand must stay false.
    // Hold the connection open so the server doesn't exit on disconnect.
    let _stdio =
        connect_and_register_with_mode(&sock_path, "echo-agent-tui", ClientMode::Stdio).await;
    // The Registered server-event is processed asynchronously after the
    // wire ack; give the server loop a beat before asserting the negative.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !*relay_demand_rx.borrow(),
        "stdio registration must not signal relay demand"
    );

    // A Headless (devbox-flow) client registers: demand flips to true.
    let _headless =
        connect_and_register_with_mode(&sock_path, "echo-agent-headless", ClientMode::Headless)
            .await;
    tokio::time::timeout(Duration::from_secs(5), relay_demand_rx.wait_for(|d| *d))
        .await
        .expect("relay demand must flip after headless registration")
        .expect("relay demand channel must stay open");

    handle.cancel.cancel();
}

#[tokio::test]
async fn client_registration_flow() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, _acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();

    let response: ServerMessage = read_message(&mut reader).await.unwrap();
    match response {
        ServerMessage::Registered {
            client_id,
            ready,
            leader_protocol_version,
            leader_binary_version,
            leader_capabilities,
        } => {
            assert!(ready);
            assert!(client_id > 0);
            assert_eq!(leader_protocol_version, Some(LEADER_PROTOCOL_VERSION));
            assert_eq!(
                leader_binary_version.as_deref(),
                Some(env!("CARGO_PKG_VERSION"))
            );
            let capabilities = leader_capabilities.expect("leader capabilities metadata");
            assert!(capabilities.control_v1);
            assert_eq!(
                capabilities.runtime_cpu_profile,
                CpuProfileManager::new().runtime_cpu_profile()
            );
        }
        _ => panic!("Expected Registered response"),
    }

    cancel.cancel();
}

#[tokio::test]
async fn control_requests_bypass_acp_routing() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("test.sock");

    let mut handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    write_message(
        &mut writer,
        &ClientMessage::Control {
            request_id: "status-1".into(),
            command: ControlCommand::CpuProfileStatus,
        },
    )
    .await
    .unwrap();

    let response: ServerMessage = read_message(&mut reader).await.unwrap();
    assert!(matches!(
        response,
        ServerMessage::ControlResult {
            request_id,
            result: Ok(ControlPayload::CpuProfileStatus {
                active: false,
                stopping: false,
                started_at: None,
                svg_path: None,
                frequency_hz: None,
            }),
        } if request_id == "status-1"
    ));

    assert!(
        tokio::time::timeout(Duration::from_millis(100), handle.acp_rx.recv())
            .await
            .is_err()
    );

    handle.cancel.cancel();
}

#[tokio::test]
async fn shutdown_waits_for_in_flight_cpu_profile_stop() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("test.sock");
    let output_path = temp.path().join("shutdown-runtime-profile.folded");
    let control_state = default_test_control_state(&sock_path);

    let stop_handle = {
        let mut manager = control_state.cpu_profile.lock();
        if !manager.runtime_cpu_profile() {
            return;
        }
        let Ok(_) = manager.start(CpuProfileStartOptions {
            output: Some(output_path.clone()),
            frequency_hz: Some(200),
        }) else {
            return;
        };
        manager.take_stop_handle().unwrap()
    };

    let control_state_for_shutdown = control_state.clone();
    let shutdown_wait = tokio::spawn(async move {
        finalize_cpu_profile_on_shutdown(control_state_for_shutdown).await;
    });

    let control_state_for_stop = control_state.clone();
    let in_flight_stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let result = tokio::task::spawn_blocking(move || stop_handle.finish())
            .await
            .unwrap()
            .unwrap();
        control_state_for_stop.cpu_profile.lock().complete_stop();
        result
    });

    tokio::time::timeout(Duration::from_secs(5), shutdown_wait)
        .await
        .expect("shutdown wait should complete")
        .unwrap();
    let stop_result = tokio::time::timeout(Duration::from_secs(5), in_flight_stop)
        .await
        .expect("in-flight stop should complete")
        .unwrap();

    assert_eq!(stop_result.svg_path, output_path);
    assert!(output_path.exists());
    assert!(matches!(
        control_state.cpu_profile.lock().status(),
        CpuProfileStatus::Inactive
    ));
}

#[tokio::test]
async fn runtime_profile_reports_unsupported_build_end_to_end() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("leader-unsupported.sock");
    let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    {
        let mut manager = handle.control_state.cpu_profile.lock();
        manager.force_unsupported_for_test();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = super::super::client::LeaderClient::connect(
        sock_path,
        "client",
        ClientMode::Stdio,
        ClientCapabilities::default(),
    )
    .await
    .unwrap();

    let runtime_cpu_profile = client
        .registration()
        .leader_capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.runtime_cpu_profile);
    assert!(
        !runtime_cpu_profile,
        "unsupported stub server must report runtime_cpu_profile=false"
    );

    let status = client
        .send_control(ControlCommand::CpuProfileStatus)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        status,
        ControlPayload::CpuProfileStatus {
            active: false,
            stopping: false,
            started_at: None,
            svg_path: None,
            frequency_hz: None,
        }
    ));

    let start_err = client
        .send_control(ControlCommand::StartCpuProfile {
            output: None,
            frequency_hz: None,
        })
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        start_err.code,
        crate::cpu_profile::ControlErrorCode::RuntimeProfilingUnsupported
    );

    let stop_err = client
        .send_control(ControlCommand::StopCpuProfile)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        stop_err.code,
        crate::cpu_profile::ControlErrorCode::ProfileNotActive
    );

    client.cancel();
    handle.cancel.cancel();
}

#[tokio::test]
async fn ping_pong() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, _acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register first
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Ping
    write_message(&mut writer, &ClientMessage::Ping)
        .await
        .unwrap();

    let response: ServerMessage = read_message(&mut reader).await.unwrap();
    assert!(matches!(response, ServerMessage::Pong));

    cancel.cancel();
}

#[tokio::test]
async fn acp_message_forwarding() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, mut acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send ACP message
    let payload = r#"{"jsonrpc":"2.0","method":"test"}"#;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: payload.into(),
        },
    )
    .await
    .unwrap();

    // Verify it was forwarded
    let received = acp_rx.recv().await.unwrap();
    assert_eq!(received, payload);

    cancel.cancel();
}

#[tokio::test]
async fn initialize_gets_client_identifier_injected() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, mut acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register with client_type "echo-agent-tui"
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "echo-agent-tui".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send initialize without clientIdentifier
    let payload =
        r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1"}}"#;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: payload.into(),
        },
    )
    .await
    .unwrap();

    // Verify the forwarded message has clientIdentifier injected
    let received = acp_rx.recv().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&received).unwrap();
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"], "echo-agent-tui",
        "Leader should inject clientIdentifier from IPC registration"
    );
    // method and id should still be present (id is namespaced)
    assert_eq!(json["method"], "initialize");

    cancel.cancel();
}

#[tokio::test]
async fn initialize_preserves_existing_client_identifier() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, mut acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register with client_type "echo-agent-tui"
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "echo-agent-tui".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send initialize WITH clientIdentifier already set
    let payload = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1","_meta":{"clientIdentifier":"echo-agent-web"}}}"#;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: payload.into(),
        },
    )
    .await
    .unwrap();

    // Verify the forwarded message kept the original clientIdentifier
    let received = acp_rx.recv().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&received).unwrap();
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"], "echo-agent-web",
        "Leader should not override existing clientIdentifier"
    );

    cancel.cancel();
}

#[test]
fn rewrite_request_id_rewrites_requests() {
    // JSON-RPC Request: has "method" and "id" -> should rewrite
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"test","id":42,"params":{}}"#);
    let client_id = ClientId(123);
    let (namespaced_id, original_id) = rewrite_request_id(&mut json, client_id).unwrap();

    assert_eq!(original_id, serde_json::json!(42));
    assert_eq!(namespaced_id, "123|42");
    assert_eq!(json["id"], "123|42");
    assert_eq!(json["method"], "test");
}

#[test]
fn is_session_attach_request_detects_load_and_resume() {
    assert!(is_session_attach_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/load","params":{"sessionId":"s1","cwd":"/tmp"}}"#
    )));
    // Resume attaches too, so it needs the same live buffering and the
    // pending-modal replay that fires on the response.
    assert!(is_session_attach_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/resume","params":{"sessionId":"s1","cwd":"/tmp"}}"#
    )));
    assert!(!is_session_attach_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}"#
    )));
    assert!(!is_session_attach_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"s1"}}"#
    )));
    // A response (no "method") is not a request.
    assert!(!is_session_attach_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
    )));
}

#[test]
fn is_scheduled_task_inject_prompt_detects_only_inject() {
    assert!(is_scheduled_task_inject_prompt(&pv(
        r#"{"method":"echo.agent/scheduled_task_inject_prompt","params":{"sessionId":"s1","taskId":"t1","prompt":"echo hi"}}"#
    )));
    // Gateway-wrapped form (the actual wire shape): `_`-prefixed top-level
    // method with the real method + params nested under `params`.
    assert!(is_scheduled_task_inject_prompt(&pv(
        r#"{"method":"_echo.agent/scheduled_task_inject_prompt","params":{"method":"echo.agent/scheduled_task_inject_prompt","params":{"sessionId":"s1","taskId":"t1","prompt":"echo hi"}}}"#
    )));
    // The sibling informational notification is NOT driver-routed (it fans
    // out so every dashboard updates its tasks pane).
    assert!(!is_scheduled_task_inject_prompt(&pv(
        r#"{"method":"echo.agent/scheduled_task_fired","params":{"sessionId":"s1"}}"#
    )));
    assert!(!is_scheduled_task_inject_prompt(&pv(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1"}}"#
    )));
}

#[test]
fn is_interaction_request_detects_only_interaction_methods() {
    for m in [
        "session/request_permission",
        "echo.agent/ask_user_question",
        "echo.agent/exit_plan_mode",
        "echo.agent/mcp/elicit",
    ] {
        let payload = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{m}","params":{{}}}}"#);
        assert!(
            is_interaction_request(&pv(&payload)),
            "{m} (direct) must be an interaction"
        );
    }
    // Gateway-wrapped ext methods (the actual wire shape for ask_user_question
    // / exit_plan_mode): `_`-prefixed top-level method, real method nested.
    for m in [
        "echo.agent/ask_user_question",
        "echo.agent/exit_plan_mode",
        "echo.agent/mcp/elicit",
    ] {
        let payload = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"_{m}","params":{{"method":"{m}","params":{{}}}}}}"#
        );
        assert!(
            is_interaction_request(&pv(&payload)),
            "wrapped {m} must be an interaction"
        );
    }
    // A regular reverse-request is NOT a shared interaction (driver-only).
    assert!(!is_interaction_request(&pv(
        r#"{"jsonrpc":"2.0","id":1,"method":"fs/read_text_file","params":{}}"#
    )));
    // `is_interaction_request` keys only on method; the caller gates on
    // `is_reverse_request` (id present) before treating it as a shared modal.
    assert!(!is_interaction_request(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/sessions/changed","params":{}}"#
    )));
}

#[test]
fn extract_interaction_tool_call_id_handles_direct_and_nested() {
    // ext-methods carry it directly under params.
    assert_eq!(
        extract_interaction_tool_call_id(&pv(
            r#"{"id":1,"method":"echo.agent/ask_user_question","params":{"sessionId":"s","toolCallId":"tc-q"}}"#
        ))
        .as_deref(),
        Some("tc-q")
    );
    // request_permission nests it under params.toolCall.
    assert_eq!(
        extract_interaction_tool_call_id(&pv(
            r#"{"id":1,"method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"tc-p"}}}"#
        ))
        .as_deref(),
        Some("tc-p")
    );
    // Gateway-wrapped ask_user_question: real toolCallId lives at
    // params.params.toolCallId.
    assert_eq!(
        extract_interaction_tool_call_id(&pv(
            r#"{"id":1,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"s","toolCallId":"tc-w"}}}"#
        ))
        .as_deref(),
        Some("tc-w")
    );
    assert_eq!(
        extract_interaction_tool_call_id(&pv(r#"{"params":{}}"#)),
        None
    );
}

#[test]
fn extract_interaction_resolved_tool_call_id_matches_only_resolved() {
    assert_eq!(
        extract_interaction_resolved_tool_call_id(&pv(
            r#"{"method":"echo.agent/session_notification","params":{"sessionId":"s","update":{"sessionUpdate":"interaction_resolved","tool_call_id":"tc-r"}}}"#
        ))
        .as_deref(),
        Some("tc-r")
    );
    // Gateway-wrapped form (the actual wire shape).
    assert_eq!(
        extract_interaction_resolved_tool_call_id(&pv(
            r#"{"method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"s","update":{"sessionUpdate":"interaction_resolved","tool_call_id":"tc-rw"}}}}"#
        ))
        .as_deref(),
        Some("tc-rw")
    );
    // A different session update is not a resolution.
    assert_eq!(
        extract_interaction_resolved_tool_call_id(&pv(
            r#"{"method":"echo.agent/session_notification","params":{"sessionId":"s","update":{"sessionUpdate":"pending_interaction","tool_call_id":"tc-r","kind":"permission"}}}"#
        )),
        None
    );
}

#[test]
fn session_load_request_id_matches_response_id_for_buffer_flush() {
    // The live-buffer flush keys on the namespaced load-request id and
    // matches it against the raw id echoed on the load response. Pin that
    // invariant: the id stored at request time equals the id seen at
    // response time, and parse_response_id still recovers (client, orig id).
    let mut req = pv(
        r#"{"jsonrpc":"2.0","id":7,"method":"session/load","params":{"sessionId":"sess-x","cwd":"/tmp"}}"#,
    );
    assert!(is_session_attach_request(&req));
    assert_eq!(extract_session_id(&req).as_deref(), Some("sess-x"));

    let client = ClientId(3);
    let (stored_ns_id, _orig) = rewrite_request_id(&mut req, client).unwrap();
    assert_eq!(stored_ns_id, "3|7");
    // The rewritten payload carries exactly the namespaced id the loop stores.
    assert_eq!(req["id"], stored_ns_id.as_str());

    // The agent echoes the namespaced id verbatim on the response;
    // `parse_response_id` recovers (client, namespaced id) and restores
    // the original id in place.
    let mut response = pv(&format!(
        r#"{{"jsonrpc":"2.0","id":"{stored_ns_id}","result":{{"models":[]}}}}"#
    ));
    let (parsed_client, raw_response_id) = parse_response_id(&mut response).unwrap();
    assert_eq!(parsed_client, client);
    assert_eq!(raw_response_id, stored_ns_id);
    assert_eq!(response["id"], serde_json::json!(7));
}

#[test]
fn live_buffer_holds_during_load_and_flushes_in_order() {
    // Mirrors the request/intercept/flush/disconnect bookkeeping the leader
    // loop performs on `pending_load_by_req` + `load_live_buffer`.
    let client = ClientId(5);
    let sid = "sess-y".to_string();
    let mut pending_load_by_req: HashMap<String, (ClientId, String)> = HashMap::new();
    let mut load_live_buffer: HashMap<(ClientId, String), Vec<BufferedLive>> = HashMap::new();

    // Request path: register pending load + empty buffer.
    pending_load_by_req.insert("5|1".to_string(), (client, sid.clone()));
    load_live_buffer.entry((client, sid.clone())).or_default();

    // Intercept path: live broadcasts during the load are buffered in order.
    for p in ["e1", "e2", "e3"] {
        if let Some(buf) = load_live_buffer.get_mut(&(client, sid.clone())) {
            buf.push((Arc::from(p), None));
        }
    }
    assert_eq!(
        load_live_buffer
            .get(&(client, sid.clone()))
            .unwrap()
            .iter()
            .map(|(p, _)| p.as_ref())
            .collect::<Vec<_>>(),
        ["e1", "e2", "e3"]
    );

    // Flush path: matching response id flushes (in order) + clears state.
    let flushed = pending_load_by_req
        .remove("5|1")
        .and_then(|(c, s)| load_live_buffer.remove(&(c, s)))
        .unwrap();
    assert_eq!(
        flushed.iter().map(|(p, _)| p.as_ref()).collect::<Vec<_>>(),
        ["e1", "e2", "e3"]
    );
    assert!(pending_load_by_req.is_empty());
    assert!(load_live_buffer.is_empty());

    // A non-matching response id leaves state untouched (no false flush).
    pending_load_by_req.insert("5|2".to_string(), (client, sid.clone()));
    load_live_buffer.entry((client, sid.clone())).or_default();
    assert!(pending_load_by_req.remove("9|9").is_none());
    assert!(load_live_buffer.contains_key(&(client, sid.clone())));

    // Disconnect path: the gone client's state is dropped.
    pending_load_by_req.retain(|_, (c, _)| *c != client);
    load_live_buffer.retain(|(c, _), _| *c != client);
    assert!(pending_load_by_req.is_empty());
    assert!(load_live_buffer.is_empty());
}

/// An `agent_message_chunk` `session/update` carrying `eventId` at
/// `params._meta.eventId` (the live-broadcast wire shape).
fn live_chunk(sid: &str, seq: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"{sid}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"x"}}}},"_meta":{{"eventId":"{sid}-{seq}"}}}}}}"#
    )
}

#[test]
fn event_seq_of_parses_acp_and_ext_and_handles_missing() {
    // ACP session/update: eventId at params._meta.eventId. The session id
    // itself contains '-', so the suffix parse must split on the LAST '-'.
    let acp = pv(r#"{"params":{"sessionId":"019e-aa","_meta":{"eventId":"019e-aa-42"}}}"#);
    assert_eq!(event_seq_of(&acp), Some(42));
    // ExtNotification (EchoAgent): nested under params.params._meta.eventId.
    let ext = pv(
        r#"{"params":{"method":"echo.agent/session/update","params":{"sessionId":"019e-aa","_meta":{"eventId":"019e-aa-7"}}}}"#,
    );
    assert_eq!(event_seq_of(&ext), Some(7));
    // No eventId (EchoAgent one-shot / older shell) → None (not dedup-droppable).
    let none = pv(r#"{"params":{"sessionId":"019e-aa","_meta":{}}}"#);
    assert_eq!(event_seq_of(&none), None);
}

/// Regression: on a mid-turn attach, the in-flight turn streams + persists
/// during the [subscribe -> gate-close] window, so its chunks are BOTH
/// buffered-live for the loading client AND read back by replay (same
/// eventId). The post-load flush must drop the buffered copies that replay
/// already delivered (`event_seq <= replay max`) and forward only the
/// genuinely-newer tail — so each event reaches the client exactly once.
#[test]
fn buffer_flush_drops_replay_overlap_by_event_seq() {
    let client = ClientId(5);
    let sid = "sess-z".to_string();
    let mut load_live_buffer: HashMap<(ClientId, String), Vec<BufferedLive>> = HashMap::new();
    let mut load_replay_max_seq: HashMap<(ClientId, String), u64> = HashMap::new();

    // Unicast replay path records the max seq delivered to this client (7..=21).
    for seq in 7..=21u64 {
        let json = pv(&live_chunk(&sid, seq));
        if let Some(s) = extract_session_id(&json)
            && let Some(n) = event_seq_of(&json)
        {
            let e = load_replay_max_seq.entry((client, s)).or_insert(0);
            *e = (*e).max(n);
        }
    }
    assert_eq!(load_replay_max_seq.get(&(client, sid.clone())), Some(&21));

    // Buffered-live holds the overlap (7..=21) AND the genuine tail (22,23).
    // Mirrors the production intercept: the seq is computed at buffer time
    // (from the already-parsed message) and stored alongside the payload.
    let buf = load_live_buffer.entry((client, sid.clone())).or_default();
    for seq in 7..=23u64 {
        let payload = live_chunk(&sid, seq);
        let event_seq = event_seq_of(&pv(&payload));
        buf.push((payload.into(), event_seq));
    }

    // Flush filter (mirrors the leader loop): drop seq <= cutoff, keep rest.
    let cutoff: Option<u64> = load_replay_max_seq.remove(&(client, sid.clone()));
    let buffered = load_live_buffer.remove(&(client, sid.clone())).unwrap();
    // Mirror the production for-loop: drop the overlap, forward the rest —
    // using only the stored seq (the flush never re-parses payloads).
    let mut forwarded: Vec<u64> = Vec::new();
    for (_, buffered_seq) in &buffered {
        if let Some(c) = cutoff
            && buffered_seq.is_some_and(|s| s <= c)
        {
            continue;
        }
        if let Some(s) = buffered_seq {
            forwarded.push(*s);
        }
    }
    assert_eq!(
        forwarded,
        vec![22, 23],
        "only the post-replay tail is forwarded (overlap 7..=21 dropped)"
    );
}

/// Edge case: a fresh process's very first event has `event_seq == 0`. The
/// cutoff must be an `Option` (not a `> 0` sentinel), so a genuine max of 0
/// still drops the buffered-live seq-0 duplicate instead of forwarding it.
#[test]
fn buffer_flush_drops_replay_overlap_at_seq_zero() {
    let client = ClientId(5);
    let sid = "sess-0".to_string();
    let mut load_live_buffer: HashMap<(ClientId, String), Vec<BufferedLive>> = HashMap::new();
    let mut load_replay_max_seq: HashMap<(ClientId, String), u64> = HashMap::new();

    // Replay delivered exactly one event: seq 0.
    let json = pv(&live_chunk(&sid, 0));
    if let Some(s) = extract_session_id(&json)
        && let Some(n) = event_seq_of(&json)
    {
        let e = load_replay_max_seq.entry((client, s)).or_insert(0);
        *e = (*e).max(n);
    }
    assert_eq!(load_replay_max_seq.get(&(client, sid.clone())), Some(&0));

    // Buffered-live holds the seq-0 duplicate + the genuine tail (seq 1).
    let buf = load_live_buffer.entry((client, sid.clone())).or_default();
    for seq in [0u64, 1] {
        let payload = live_chunk(&sid, seq);
        let event_seq = event_seq_of(&pv(&payload));
        buf.push((payload.into(), event_seq));
    }

    let cutoff: Option<u64> = load_replay_max_seq.remove(&(client, sid.clone()));
    assert_eq!(
        cutoff,
        Some(0),
        "a genuine cutoff of 0 must be Some(0), not absent"
    );
    let buffered = load_live_buffer.remove(&(client, sid.clone())).unwrap();
    // Mirror the production for-loop: drop the overlap, forward the rest —
    // using only the stored seq (the flush never re-parses payloads).
    let mut forwarded: Vec<u64> = Vec::new();
    for (_, buffered_seq) in &buffered {
        if let Some(c) = cutoff
            && buffered_seq.is_some_and(|s| s <= c)
        {
            continue;
        }
        if let Some(s) = buffered_seq {
            forwarded.push(*s);
        }
    }
    assert_eq!(
        forwarded,
        vec![1],
        "seq-0 duplicate dropped, seq-1 tail forwarded (Option cutoff, not > 0)"
    );
}

#[test]
fn rewrite_request_id_skips_responses_with_result() {
    // JSON-RPC Response (success): has "result" and "id", no "method" -> should NOT rewrite
    let mut json = pv(r#"{"jsonrpc":"2.0","result":{"content":"hello"},"id":42}"#);
    let before = json.clone();
    assert!(rewrite_request_id(&mut json, ClientId(123)).is_none());
    assert_eq!(json, before, "payload unchanged");
}

#[test]
fn rewrite_request_id_skips_responses_with_error() {
    // JSON-RPC Response (error): has "error" and "id", no "method" -> should NOT rewrite
    let mut json = pv(r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid"},"id":5}"#);
    let before = json.clone();
    assert!(rewrite_request_id(&mut json, ClientId(123)).is_none());
    assert_eq!(json, before, "payload unchanged");
}

#[test]
fn rewrite_request_id_handles_notifications() {
    // JSON-RPC Notification: has "method" but no "id" -> nothing to rewrite
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#);
    let before = json.clone();
    assert!(rewrite_request_id(&mut json, ClientId(123)).is_none());
    assert_eq!(json, before, "payload unchanged");
    assert!(json.get("id").is_none()); // Still no id
}

#[test]
fn rewrite_request_id_handles_string_ids() {
    // JSON-RPC Request with string id
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"test","id":"abc-123"}"#);
    let (namespaced_id, original_id) = rewrite_request_id(&mut json, ClientId(456)).unwrap();

    assert_eq!(original_id, serde_json::json!("abc-123"));
    // String IDs get JSON-serialized with quotes: "abc-123" -> "\"abc-123\""
    assert_eq!(namespaced_id, "456|\"abc-123\"");
    assert_eq!(json["id"], "456|\"abc-123\"");
}

#[test]
fn inject_capabilities_adds_yolo_mode_to_session_new() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: true,
        default_model: None,
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));

    assert_eq!(json["params"]["_meta"]["yoloMode"], true);
}

/// Leader capabilities.auto_mode seeds `_meta.autoMode` on session/new
/// (the real ConnectFlags.default_auto_mode entry path).
#[test]
fn inject_capabilities_adds_auto_mode_to_session_new() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        auto_mode: true,
        yolo_mode: false,
        default_model: None,
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    assert_eq!(json["params"]["_meta"]["autoMode"], true);
    assert!(json["params"]["_meta"].get("yoloMode").is_none());
}

/// session/load also receives autoMode (reconnect path).
#[test]
fn inject_capabilities_adds_auto_mode_to_session_load() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1"}}}}"#,
        AGENT_METHOD_NAMES.session_load
    );
    let caps = ClientCapabilities {
        auto_mode: true,
        yolo_mode: false,
        ..Default::default()
    };
    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "echo-agent-tui",
        ClientId(1)
    ));
    assert_eq!(json["params"]["_meta"]["autoMode"], true);
}

#[test]
fn inject_capabilities_adds_auto_mode_to_session_resume() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1"}}}}"#,
        AGENT_METHOD_NAMES.session_resume
    );
    let caps = ClientCapabilities {
        auto_mode: true,
        yolo_mode: false,
        ..Default::default()
    };
    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "echo-agent-tui",
        ClientId(1)
    ));
    assert_eq!(json["params"]["_meta"]["autoMode"], true);
}

/// Yolo suppresses autoMode injection even when auto_mode capability is set.
#[test]
fn inject_capabilities_yolo_suppresses_auto_mode() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        auto_mode: true,
        yolo_mode: true,
        ..Default::default()
    };
    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    assert_eq!(json["params"]["_meta"]["yoloMode"], true);
    assert!(
        json["params"]["_meta"].get("autoMode").is_none(),
        "yolo must not also inject autoMode"
    );
}

#[test]
fn inject_capabilities_preserves_explicit_auto_over_stale_yolo() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp","_meta":{{"yoloMode":false,"autoMode":true}}}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        auto_mode: false,
        yolo_mode: true,
        ..Default::default()
    };
    let mut json = pv(&payload);

    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "echo-agent-tui",
        ClientId(1)
    ));
    assert_eq!(json["params"]["_meta"]["yoloMode"], false);
    assert_eq!(json["params"]["_meta"]["autoMode"], true);
}

#[test]
fn inject_capabilities_skips_non_session_new() {
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"other/method","id":1,"params":{}}"#);
    let caps = ClientCapabilities {
        yolo_mode: true,
        default_model: None,
        ..Default::default()
    };

    assert!(!inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    // Should be unchanged (or at least not have yoloMode injected)
    assert!(json["params"].get("_meta").is_none());
}

#[test]
fn inject_capabilities_skips_when_yolo_mode_false() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: None,
        ..Default::default()
    };

    let mut json = pv(&payload);
    let before = json.clone();
    assert!(!inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    // Should be unchanged
    assert_eq!(json, before);
}

#[test]
fn inject_capabilities_adds_status_line_when_it_is_the_only_capability() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        status_line: true,
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    assert_eq!(
        json["params"]["_meta"][echo_agent_status_line::CLIENT_STATUS_LINE_META],
        true,
        "a status-line client that states nothing else must still get its own capability, \
         not the process-wide initialize one"
    );
}

#[test]
fn inject_capabilities_preserves_existing_meta() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp","_meta":{{"foo":"bar"}}}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: true,
        default_model: None,
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));

    // Should have both existing foo and new yoloMode
    assert_eq!(json["params"]["_meta"]["foo"], "bar");
    assert_eq!(json["params"]["_meta"]["yoloMode"], true);
}

#[test]
fn inject_capabilities_adds_default_model_to_session_new() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: Some("echo-agent-3-fast".to_string()),
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));

    assert_eq!(json["params"]["_meta"]["modelId"], "echo-agent-3-fast");
    // yoloMode should not be present since it's false
    assert!(json["params"]["_meta"].get("yoloMode").is_none());
}

#[test]
fn inject_capabilities_adds_both_yolo_and_model() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: true,
        default_model: Some("echo-agent-3-fast".to_string()),
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));

    assert_eq!(json["params"]["_meta"]["yoloMode"], true);
    assert_eq!(json["params"]["_meta"]["modelId"], "echo-agent-3-fast");
}

#[test]
fn inject_capabilities_does_not_override_existing_model_id() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp","_meta":{{"modelId":"custom-model"}}}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: Some("echo-agent-3-fast".to_string()),
        ..Default::default()
    };

    let mut json = pv(&payload);
    inject_session_request_context(&mut json, &caps, "", ClientId(1));

    // Should preserve the existing modelId
    assert_eq!(json["params"]["_meta"]["modelId"], "custom-model");
}

#[test]
fn extract_yolo_mode_change_returns_value() {
    let payload =
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"yolo_mode":true}}"#;
    assert_eq!(extract_yolo_mode_change(&pv(payload)), Some(true));

    let payload =
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"yolo_mode":false}}"#;
    assert_eq!(extract_yolo_mode_change(&pv(payload)), Some(false));
}

#[test]
fn extract_yolo_mode_change_returns_none_for_other_methods() {
    let payload = r#"{"jsonrpc":"2.0","method":"other/method","params":{"yolo_mode":true}}"#;
    assert_eq!(extract_yolo_mode_change(&pv(payload)), None);
}

/// Branch 1: an explicit `auto_mode` flag wins, even over `permission_mode`.
#[test]
fn extract_auto_mode_change_explicit_flag_wins() {
    let payload =
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"auto_mode":true}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), Some(true));

    let payload =
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"auto_mode":false}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), Some(false));

    // Explicit flag wins even when permission_mode would say otherwise.
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"auto_mode":false,"permission_mode":"auto"}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), Some(false));
}

/// Branch 2: with no explicit flag, derive from `permission_mode`.
#[test]
fn extract_auto_mode_change_derives_from_permission_mode() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"permission_mode":"auto"}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), Some(true));

    for mode in ["ask", "always-approve", "default"] {
        let payload = format!(
            r#"{{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{{"permission_mode":"{mode}"}}}}"#
        );
        assert_eq!(
            extract_auto_mode_change(&pv(&payload)),
            Some(false),
            "permission_mode={mode} must clear auto"
        );
    }
}

/// Branch 3: None when there's no auto signal — wrong method, or a bare yolo
/// toggle (no `auto_mode`, no `permission_mode`) must NOT change auto state.
#[test]
fn extract_auto_mode_change_returns_none_when_no_auto_signal() {
    let payload = r#"{"jsonrpc":"2.0","method":"other/method","params":{"auto_mode":true}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), None);

    let payload =
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"yolo_mode":true}}"#;
    assert_eq!(extract_auto_mode_change(&pv(payload)), None);
}

#[test]
fn extract_model_id_from_set_model_returns_value() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-123","modelId":"echo-agent-3-fast"}}}}"#,
        AGENT_METHOD_NAMES.session_set_model
    );
    assert_eq!(
        extract_model_id_from_set_model(&pv(&payload)),
        Some("echo-agent-3-fast".to_string())
    );
}

#[test]
fn extract_model_id_from_set_model_handles_snake_case() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"session_id":"sess-123","model_id":"echo-agent-3"}}}}"#,
        AGENT_METHOD_NAMES.session_set_model
    );
    assert_eq!(
        extract_model_id_from_set_model(&pv(&payload)),
        Some("echo-agent-3".to_string())
    );
}

#[test]
fn extract_model_id_from_set_model_returns_none_for_other_methods() {
    let payload =
        r#"{"jsonrpc":"2.0","method":"other/method","id":1,"params":{"modelId":"echo-agent-3"}}"#;
    assert_eq!(extract_model_id_from_set_model(&pv(payload)), None);
}

#[test]
fn extract_model_id_from_set_model_returns_none_for_empty_model() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-123","modelId":""}}}}"#,
        AGENT_METHOD_NAMES.session_set_model
    );
    assert_eq!(extract_model_id_from_set_model(&pv(&payload)), None);
}

#[test]
fn extract_model_id_from_set_model_returns_none_for_missing_model() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-123"}}}}"#,
        AGENT_METHOD_NAMES.session_set_model
    );
    assert_eq!(extract_model_id_from_set_model(&pv(&payload)), None);
}

// ── patch_initialize_response_model tests ─────────────────────────

#[test]
fn patch_initialize_response_patches_current_model_id() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"meta":{"modelState":{"currentModelId":"echo-agent-3","availableModels":[]}}}}"#,
    );
    let default_model = Some("echo-agent-3-fast".to_string());
    assert!(patch_initialize_response_model(&mut json, &default_model));
    assert_eq!(
        json["result"]["meta"]["modelState"]["currentModelId"],
        "echo-agent-3-fast"
    );
}

#[test]
fn patch_initialize_response_preserves_other_fields() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"meta":{"echoagentShell":true,"modelState":{"currentModelId":"echo-agent-3","availableModels":[{"modelId":"echo-agent-3"},{"modelId":"echo-agent-3-fast"}]}}}}"#,
    );
    let default_model = Some("echo-agent-3-fast".to_string());
    assert!(patch_initialize_response_model(&mut json, &default_model));
    assert_eq!(json["result"]["meta"]["echoagentShell"], true);
    assert_eq!(
        json["result"]["meta"]["modelState"]["currentModelId"],
        "echo-agent-3-fast"
    );
    // availableModels should be preserved
    assert_eq!(
        json["result"]["meta"]["modelState"]["availableModels"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn patch_initialize_response_noop_when_no_default_model() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"meta":{"modelState":{"currentModelId":"echo-agent-3"}}}}"#,
    );
    let before = json.clone();
    assert!(!patch_initialize_response_model(&mut json, &None));
    assert_eq!(json, before);
}

#[test]
fn patch_initialize_response_noop_when_empty_default_model() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"meta":{"modelState":{"currentModelId":"echo-agent-3"}}}}"#,
    );
    let before = json.clone();
    assert!(!patch_initialize_response_model(
        &mut json,
        &Some("".to_string())
    ));
    assert_eq!(json, before);
}

#[test]
fn patch_initialize_response_noop_when_already_matches() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"meta":{"modelState":{"currentModelId":"echo-agent-3"}}}}"#,
    );
    let before = json.clone();
    assert!(!patch_initialize_response_model(
        &mut json,
        &Some("echo-agent-3".to_string())
    ));
    assert_eq!(json, before);
}

#[test]
fn patch_initialize_response_noop_for_non_initialize_response() {
    // A session/new response has "models" not "meta.modelState"
    let mut json = pv(
        r#"{"jsonrpc":"2.0","id":1,"result":{"session_id":"sess-1","models":{"currentModelId":"echo-agent-3","availableModels":[]}}}"#,
    );
    let before = json.clone();
    assert!(!patch_initialize_response_model(
        &mut json,
        &Some("echo-agent-3-fast".to_string())
    ));
    // Should be unchanged — no meta.modelState path
    assert_eq!(json, before);
}

#[test]
fn extract_session_id_from_result_works() {
    // Session/new response with session_id in result
    let payload = r#"{"jsonrpc":"2.0","result":{"session_id":"sess-123"},"id":1}"#;
    assert_eq!(
        extract_session_id_from_result(&pv(payload)),
        Some("sess-123".to_string())
    );

    // Also works with camelCase sessionId
    let payload = r#"{"jsonrpc":"2.0","result":{"sessionId":"sess-456"},"id":1}"#;
    assert_eq!(
        extract_session_id_from_result(&pv(payload)),
        Some("sess-456".to_string())
    );
}

#[test]
fn extract_session_id_from_result_returns_none_for_other_responses() {
    // Response without session_id
    let payload = r#"{"jsonrpc":"2.0","result":{"other":"value"},"id":1}"#;
    assert_eq!(extract_session_id_from_result(&pv(payload)), None);

    // Error response
    let payload = r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"fail"},"id":1}"#;
    assert_eq!(extract_session_id_from_result(&pv(payload)), None);

    // Request (not a response)
    let payload = r#"{"jsonrpc":"2.0","method":"test","params":{"session_id":"abc"},"id":1}"#;
    assert_eq!(extract_session_id_from_result(&pv(payload)), None);
}

#[test]
fn extract_session_id_from_params_works() {
    // Notification with session_id in params
    let payload =
        r#"{"jsonrpc":"2.0","method":"session/notification","params":{"session_id":"sess-789"}}"#;
    assert_eq!(
        extract_session_id(&pv(payload)),
        Some("sess-789".to_string())
    );

    // Also works with camelCase sessionId
    let payload =
        r#"{"jsonrpc":"2.0","method":"session/notification","params":{"sessionId":"sess-abc"}}"#;
    assert_eq!(
        extract_session_id(&pv(payload)),
        Some("sess-abc".to_string())
    );
}

#[test]
fn extract_session_id_from_nested_params_works() {
    // ext/notification: sessionId is nested inside params.params
    // Wire format: {"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"sess-nested"}}}
    let payload = r#"{"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"sess-nested"}}}"#;
    assert_eq!(
        extract_session_id(&pv(payload)),
        Some("sess-nested".to_string())
    );

    // Also works with snake_case session_id in nested params
    let payload = r#"{"jsonrpc":"2.0","method":"_echo.agent/fs_notify","params":{"method":"echo.agent/fs_notify","params":{"session_id":"sess-nested-2","event":{}}}}"#;
    assert_eq!(
        extract_session_id(&pv(payload)),
        Some("sess-nested-2".to_string())
    );

    // Top-level sessionId takes precedence over nested
    let payload = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"top-level","params":{"sessionId":"nested"}}}"#;
    assert_eq!(
        extract_session_id(&pv(payload)),
        Some("top-level".to_string())
    );
}

#[test]
fn extract_session_id_from_prompt_complete_works() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session/prompt_complete","params":{"sessionId":"sess-prompt"}}"#;
    assert_eq!(
        extract_session_id_from_prompt_complete(&pv(payload)),
        Some("sess-prompt".to_string())
    );
}

#[test]
fn extract_session_id_from_prompt_complete_ignores_other_methods() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-prompt"}}"#;
    assert_eq!(extract_session_id_from_prompt_complete(&pv(payload)), None);
}

#[test]
fn extract_child_session_event_spawned() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-1"}}}"#;
    match extract_child_session_event(&pv(payload)) {
        Some(ChildSessionEvent::Spawned(id)) => assert_eq!(id, "child-1"),
        other => panic!("Expected Spawned, got {:?}", other),
    }
}

#[test]
fn extract_child_session_event_finished() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-2"}}}"#;
    match extract_child_session_event(&pv(payload)) {
        Some(ChildSessionEvent::Finished(id)) => assert_eq!(id, "child-2"),
        other => panic!("Expected Finished, got {:?}", other),
    }
}

#[test]
fn extract_child_session_event_nested_ext_notification() {
    let payload = r#"{"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-3"}}}}"#;
    match extract_child_session_event(&pv(payload)) {
        Some(ChildSessionEvent::Spawned(id)) => assert_eq!(id, "child-3"),
        other => panic!("Expected Spawned, got {:?}", other),
    }
}

#[test]
fn extract_child_session_event_nested_ext_notification_finished() {
    let payload = r#"{"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-4"}}}}"#;
    match extract_child_session_event(&pv(payload)) {
        Some(ChildSessionEvent::Finished(id)) => assert_eq!(id, "child-4"),
        other => panic!("Expected Finished, got {:?}", other),
    }
}

#[test]
fn extract_child_session_event_none_for_other_updates() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"message_delta","content":"hello"}}}"#;
    assert!(extract_child_session_event(&pv(payload)).is_none());
}

#[test]
fn extract_child_session_event_none_without_child_id() {
    let payload = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"parent","update":{"sessionUpdate":"subagent_spawned"}}}"#;
    assert!(extract_child_session_event(&pv(payload)).is_none());
}

#[test]
fn inject_capabilities_skips_empty_default_model() {
    // When default_model is Some(""), it should NOT inject modelId
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: Some("".to_string()),
        ..Default::default()
    };

    let mut json = pv(&payload);
    let before = json.clone();
    assert!(!inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    // With empty default_model and yolo_mode=false, the payload should be unchanged
    assert_eq!(json, before);
}

#[test]
fn inject_capabilities_skips_empty_model_with_yolo_mode() {
    // When default_model is Some("") but yolo_mode is true,
    // yoloMode should be injected but modelId should NOT
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: true,
        default_model: Some("".to_string()),
        ..Default::default()
    };

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));

    assert_eq!(json["params"]["_meta"]["yoloMode"], true);
    // modelId should NOT be present since default_model is empty
    assert!(json["params"]["_meta"].get("modelId").is_none());
}

#[test]
fn inject_capabilities_no_model_no_yolo_returns_unchanged() {
    // When neither yolo_mode nor default_model is set, payload is returned unchanged
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp","_meta":{{"yoloMode":true}}}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: None,
        ..Default::default()
    };

    let mut json = pv(&payload);
    let before = json.clone();
    assert!(!inject_session_request_context(
        &mut json,
        &caps,
        "",
        ClientId(1)
    ));
    assert_eq!(json, before);
}

#[test]
fn inject_capabilities_adds_client_identifier_to_session_new() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities::default();

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "echo-agent-code-extension",
        ClientId(1),
    ));
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"],
        "echo-agent-code-extension"
    );
}

#[test]
fn inject_capabilities_does_not_override_existing_client_identifier() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"cwd":"/tmp","_meta":{{"clientIdentifier":"custom-client"}}}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    let caps = ClientCapabilities::default();

    let mut json = pv(&payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-tui", ClientId(1));
    // Should preserve the existing clientIdentifier
    assert_eq!(json["params"]["_meta"]["clientIdentifier"], "custom-client");
}

#[test]
fn inject_capabilities_adds_client_identifier_to_session_load() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1"}}}}"#,
        AGENT_METHOD_NAMES.session_load
    );
    let caps = ClientCapabilities::default();

    let mut json = pv(&payload);
    assert!(inject_session_request_context(
        &mut json,
        &caps,
        "echo-agent-code-extension",
        ClientId(1),
    ));
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"],
        "echo-agent-code-extension"
    );
    // session/load should NOT get yoloMode or modelId injected
    assert!(json["params"]["_meta"].get("yoloMode").is_none());
    assert!(json["params"]["_meta"].get("modelId").is_none());
}

#[test]
fn inject_capabilities_adds_leader_client_id_to_session_load() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1"}}}}"#,
        AGENT_METHOD_NAMES.session_load
    );
    let caps = ClientCapabilities::default();

    let mut json = pv(&payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-tui", ClientId(42));
    // The unique ClientId is stamped so the agent can echo it onto replay
    // notifications for leader unicast routing.
    assert_eq!(
        json["params"]["_meta"]["echo.agent/leaderClientId"].as_u64(),
        Some(42)
    );
}

#[test]
fn inject_capabilities_does_not_override_existing_leader_client_id() {
    let payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1","_meta":{{"echo.agent/leaderClientId":7}}}}}}"#,
        AGENT_METHOD_NAMES.session_load
    );
    let caps = ClientCapabilities::default();

    let mut json = pv(&payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-tui", ClientId(42));
    // An explicit value already present is respected (mirrors the
    // clientIdentifier guard).
    assert_eq!(
        json["params"]["_meta"]["echo.agent/leaderClientId"].as_u64(),
        Some(7)
    );
}

#[test]
fn extract_target_client_id_some_when_meta_present() {
    // SessionNotification shape: _meta lives directly under params.
    let direct = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","_meta":{"echo.agent/leaderClientId":9}}}"#;
    assert_eq!(extract_target_client_id(&pv(direct)), Some(ClientId(9)));

    // ExtNotification shape: real params (and _meta) nested under params.params.
    let nested = r#"{"jsonrpc":"2.0","method":"_echo.agent/session/update","params":{"params":{"sessionId":"sess-1","_meta":{"echo.agent/leaderClientId":11}}}}"#;
    assert_eq!(extract_target_client_id(&pv(nested)), Some(ClientId(11)));
}

#[test]
fn extract_target_client_id_none_when_absent() {
    // No _meta at all.
    let no_meta = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1"}}"#;
    assert_eq!(extract_target_client_id(&pv(no_meta)), None);

    // _meta present but no leaderClientId key (e.g. a live, untagged delta).
    let no_key = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","_meta":{"isReplay":true}}}"#;
    assert_eq!(extract_target_client_id(&pv(no_key)), None);
}

#[test]
fn inject_yolo_notification_adds_client_identifier() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/yolo_mode_changed","params":{"yolo_mode":true}}"#,
    );

    assert!(inject_client_identity_into_yolo_notification(
        &mut json,
        "echo-agent-tui"
    ));
    assert_eq!(json["params"]["clientIdentifier"], "echo-agent-tui");
    assert_eq!(json["params"]["yolo_mode"], true);
}

#[test]
fn inject_yolo_notification_skips_non_yolo_methods() {
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"echo.agent/other","params":{"data":1}}"#);
    let before = json.clone();

    assert!(!inject_client_identity_into_yolo_notification(
        &mut json,
        "echo-agent-tui"
    ));
    assert_eq!(json, before);
}

#[test]
fn inject_client_identity_adds_identifier_to_initialize() {
    let mut json =
        pv(r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1"}}"#);

    let (mutated, was_initialize) =
        inject_client_identity_into_initialize(&mut json, "echo-agent-tui");
    assert!(was_initialize, "should have detected an initialize message");
    assert!(mutated, "should have injected the identifier");
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"],
        "echo-agent-tui"
    );
}

#[test]
fn inject_client_identity_does_not_override_existing() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1","_meta":{"clientIdentifier":"echo-agent-web"}}}"#,
    );

    let (mutated, was_initialize) =
        inject_client_identity_into_initialize(&mut json, "echo-agent-tui");
    assert!(was_initialize, "should have detected an initialize message");
    assert!(!mutated, "existing identifier means nothing was injected");
    // Should preserve the existing clientIdentifier
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"],
        "echo-agent-web"
    );
}

#[test]
fn inject_client_identity_skips_non_initialize() {
    let mut json = pv(r#"{"jsonrpc":"2.0","method":"session/new","id":1,"params":{"cwd":"/tmp"}}"#);
    let before = json.clone();

    let (mutated, was_initialize) =
        inject_client_identity_into_initialize(&mut json, "echo-agent-tui");
    assert!(
        !was_initialize,
        "session/new should not be detected as initialize"
    );
    assert!(!mutated);
    assert_eq!(json, before);
}

#[test]
fn inject_client_identity_skips_empty_client_type() {
    let mut json =
        pv(r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1"}}"#);
    let before = json.clone();

    let (mutated, was_initialize) = inject_client_identity_into_initialize(&mut json, "");
    assert!(
        !was_initialize,
        "empty client_type means no injection, not an initialize"
    );
    assert!(!mutated);
    assert_eq!(json, before);
}

#[test]
fn inject_client_identity_preserves_existing_meta() {
    let mut json = pv(
        r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"0.1","_meta":{"foo":"bar"}}}"#,
    );

    let (mutated, was_initialize) =
        inject_client_identity_into_initialize(&mut json, "echo-agent-code-extension");
    assert!(was_initialize, "should have detected an initialize message");
    assert!(mutated);
    // Should have both existing foo and new clientIdentifier
    assert_eq!(json["params"]["_meta"]["foo"], "bar");
    assert_eq!(
        json["params"]["_meta"]["clientIdentifier"],
        "echo-agent-code-extension"
    );
}

// ── make_version_mismatch_notification unit tests ─────────────────────────

#[test]
fn version_mismatch_notification_contains_correct_fields() {
    let payload = make_version_mismatch_notification("0.1.157", "0.1.150")
        .expect("should produce notification");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(json["method"], "echo.agent/leader/version_mismatch");
    assert_eq!(json["params"]["clientVersion"], "0.1.157");
    assert_eq!(json["params"]["leaderVersion"], "0.1.150");
    assert!(
        json["params"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("0.1.157"),
        "message should mention the client version"
    );
}

#[test]
fn version_mismatch_notification_is_none_when_versions_match() {
    assert!(
        make_version_mismatch_notification("0.1.150", "0.1.150").is_none(),
        "matching versions must not produce a notification"
    );
}

#[test]
fn version_mismatch_notification_is_none_for_unknown_leader_version() {
    assert!(
        make_version_mismatch_notification("0.1.150", "unknown").is_none(),
        "unknown leader version (dev build) must not produce a notification"
    );
}

/// Verify that a session/setModel request updates the client's default_model
/// capability, so the next session/new injects the updated model.
#[tokio::test]
async fn set_model_updates_default_model_for_next_session_new() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, mut acp_rx) = setup_test_server(&temp).await;

    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Register with an initial default_model of "echo-agent-original"
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities {
                yolo_mode: false,
                default_model: Some("echo-agent-original".to_string()),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // 1. Send session/setModel to switch to "echo-agent-4.5"
    let set_model_payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":1,"params":{{"sessionId":"sess-1","modelId":"echo-agent-4.5"}}}}"#,
        AGENT_METHOD_NAMES.session_set_model
    );
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: set_model_payload,
        },
    )
    .await
    .unwrap();
    // Consume the forwarded message
    let _ = acp_rx.recv().await.unwrap();

    // 2. Send session/new — the leader should inject the UPDATED model ("echo-agent-4.5")
    let session_new_payload = format!(
        r#"{{"jsonrpc":"2.0","method":"{}","id":2,"params":{{"cwd":"/tmp"}}}}"#,
        AGENT_METHOD_NAMES.session_new
    );
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: session_new_payload,
        },
    )
    .await
    .unwrap();

    let forwarded = acp_rx.recv().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&forwarded).unwrap();

    // The injected modelId should be the NEW model, not the original registration model
    assert_eq!(
        json["params"]["_meta"]["modelId"], "echo-agent-4.5",
        "Leader should inject the updated model after session/setModel, not the stale registration model"
    );

    cancel.cancel();
}

#[tokio::test]
async fn client_count_starts_at_zero() {
    let temp = TempDir::new().unwrap();
    let (_sock_path, cancel, _acp_rx, client_count) =
        setup_test_server_with_client_count(&temp).await;

    assert_eq!(
        client_count.load(Ordering::Relaxed),
        0,
        "client_count should start at 0"
    );

    cancel.cancel();
}

#[tokio::test]
async fn client_count_increments_on_connect() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, _acp_rx, client_count) =
        setup_test_server_with_client_count(&temp).await;

    // Connect one client
    let (_reader1, _writer1) = connect_and_register(&sock_path, "client-1").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        client_count.load(Ordering::Relaxed),
        1,
        "client_count should be 1 after one client connects"
    );

    // Connect a second client
    let (_reader2, _writer2) = connect_and_register(&sock_path, "client-2").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        client_count.load(Ordering::Relaxed),
        2,
        "client_count should be 2 after two clients connect"
    );

    cancel.cancel();
}

#[tokio::test]
async fn client_count_decrements_on_disconnect() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, _acp_rx, client_count) =
        setup_test_server_with_client_count(&temp).await;

    // Connect two clients
    let (_reader1, mut writer1) = connect_and_register(&sock_path, "client-1").await;
    let (_reader2, _writer2) = connect_and_register(&sock_path, "client-2").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(client_count.load(Ordering::Relaxed), 2);

    // Disconnect first client by sending Disconnect message
    write_message(&mut writer1, &ClientMessage::Disconnect)
        .await
        .unwrap();
    drop(_reader1);
    drop(writer1);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        client_count.load(Ordering::Relaxed),
        1,
        "client_count should be 1 after one client disconnects"
    );

    cancel.cancel();
}

#[tokio::test]
async fn client_count_returns_to_zero_after_all_disconnect() {
    let temp = TempDir::new().unwrap();
    // Use run_leader_server directly with no_exit_on_disconnect=true
    // so the server doesn't shut down when all clients disconnect.
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, _acp_rx) = mpsc::unbounded_channel();
    let (_response_tx, response_rx) = mpsc::unbounded_channel();
    let server_cancel = CancellationToken::new();
    let client_count = Arc::new(AtomicUsize::new(0));
    let agent_busy = Arc::new(AtomicBool::new(false));
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = server_cancel.clone();
    let count_clone = client_count.clone();
    let busy_clone = agent_busy.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true, // no_exit_on_disconnect
            count_clone,
            busy_clone,
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect client
    {
        let (_reader, mut writer) = connect_and_register(&sock_path, "temp-client").await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(client_count.load(Ordering::Relaxed), 1);

        // Disconnect
        write_message(&mut writer, &ClientMessage::Disconnect)
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        client_count.load(Ordering::Relaxed),
        0,
        "client_count should return to 0 after all clients disconnect"
    );

    server_cancel.cancel();
}

#[tokio::test]
async fn client_count_not_incremented_before_registration() {
    let temp = TempDir::new().unwrap();
    let (_sock_path, cancel, _acp_rx, client_count) =
        setup_test_server_with_client_count(&temp).await;

    // Connect but do NOT register — just open the TCP connection
    let _stream = LeaderStream::connect(&_sock_path).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(
        client_count.load(Ordering::Relaxed),
        0,
        "client_count should remain 0 for unregistered connections"
    );

    cancel.cancel();
}

#[tokio::test]
async fn fallback_routing_forwards_notifications_but_drops_responses() {
    let temp = TempDir::new().unwrap();
    // Use run_leader_server directly with no_exit_on_disconnect=true
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, _acp_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let server_cancel = CancellationToken::new();
    let client_count = Arc::new(AtomicUsize::new(0));
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = server_cancel.clone();
    let count_clone = client_count.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            count_clone,
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and register an IPC client
    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send an ACP message to make this client the "last active"
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test","id":99}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate a relay-originated RESPONSE (has "id" but no namespace prefix).
    // This should be DROPPED — not forwarded to the IPC client.
    response_tx
        .send(r#"{"jsonrpc":"2.0","result":{"ok":true},"id":42}"#.to_string())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate a NOTIFICATION (no "id", no session_id match).
    // This should be forwarded to the last active IPC client.
    response_tx
        .send(
            r#"{"jsonrpc":"2.0","method":"agent/progress","params":{"status":"working"}}"#
                .to_string(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Read from the client — we should get the notification but NOT the response.
    // The notification arrives, the response was dropped.
    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .expect("should receive notification")
            .unwrap();
    match msg {
        ServerMessage::Acp { payload } => {
            let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(
                json["method"], "agent/progress",
                "Should receive the notification, not the relay response"
            );
        }
        other => panic!("Expected Acp message, got {:?}", other),
    }

    server_cancel.cancel();
}

/// Relay-originated session notifications must be dropped, not forwarded
/// to the last active IPC client.
#[tokio::test]
async fn relay_session_notification_not_forwarded_to_ipc_client() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader, mut writer) = connect_and_register(&sock_path, "test").await;

    // Make this client last_active by sending an ACP message
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test","id":99}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Notification with unregistered sessionId (relay-originated) — must be dropped
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"relay-sess-xyz","data":"from-relay"}}"#.into())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Session-less notification — should be forwarded, proving the channel is alive
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"agent/progress","params":{"status":"working"}}"#.into())
        .unwrap();

    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .expect("should receive the session-less notification")
            .unwrap();
    match msg {
        ServerMessage::Acp { payload } => {
            let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(json["method"], "agent/progress");
            assert!(json["params"].get("sessionId").is_none());
        }
        other => panic!("Expected Acp message, got {:?}", other),
    }

    cancel.cancel();
}

/// When a client disconnects while its session streams, notifications for
/// that session must NOT leak to another client via `last_active_client`.
#[tokio::test]
async fn dead_client_session_notification_not_leaked_to_other_client() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    // Client A registers ownership of "sess-A"
    let (reader_a, mut writer_a) = connect_and_register(&sock_path, "test-a").await;
    write_message(
        &mut writer_a,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-A","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Disconnect client A
    drop(writer_a);
    drop(reader_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Client B connects and becomes last_active_client
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "test-b").await;
    write_message(
        &mut writer_b,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"initialize","id":2,"params":{}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Notification for dead client A's session — must be dropped
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-A","sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"leaked content"}}}"#.into())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Session-less notification — should reach client B
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"agent/progress","params":{"status":"working"}}"#.into())
        .unwrap();

    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_b))
            .await
            .expect("should receive the session-less notification")
            .unwrap();
    match msg {
        ServerMessage::Acp { payload } => {
            let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(json["method"], "agent/progress");
        }
        other => panic!("Expected Acp message, got {:?}", other),
    }

    cancel.cancel();
}

/// `ext/notification` with nested sessionId (params.params.sessionId) must
/// route to the session owner, not fall through to `last_active_client`.
#[tokio::test]
async fn ext_notification_with_nested_session_id_routes_correctly() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    // Client A owns "sess-A"
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "test-a").await;
    write_message(
        &mut writer_a,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-A","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Client B becomes last_active_client
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "test-b").await;
    write_message(
        &mut writer_b,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"initialize","id":2,"params":{}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ext/notification with nested sessionId for session A
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"sess-A","update":{"sessionUpdate":"retry_state","attempt":1,"maxRetries":3,"reason":"transient"}}}}"#.into())
        .unwrap();

    // Client A receives it
    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_a))
            .await
            .expect("client A should receive the ext/notification")
            .unwrap();
    match msg {
        ServerMessage::Acp { payload } => {
            let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(json["method"], "_echo.agent/session_notification");
        }
        other => panic!("Expected Acp message, got {:?}", other),
    }

    // Client B does NOT receive it
    let timeout_result: Result<Result<ServerMessage, _>, _> =
        tokio::time::timeout(Duration::from_millis(100), read_message(&mut reader_b)).await;
    assert!(
        timeout_result.is_err(),
        "Client B should NOT receive session A's notification"
    );

    cancel.cancel();
}

#[tokio::test]
async fn server_sends_shutting_down_before_shutdown() {
    let temp = TempDir::new().unwrap();
    // Use run_leader_server directly with no_exit_on_disconnect=true
    // so the server doesn't shut down when the client disconnects.
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, _acp_rx) = mpsc::unbounded_channel();
    let (_response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let client_count = Arc::new(AtomicUsize::new(0));
    let control_state = default_test_control_state(&sock_path);

    let cancel_clone = cancel.clone();
    let sock_clone = sock_path.clone();
    let cc = client_count.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            cc,
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and register a raw client
    let (mut reader, _writer) = connect_and_register(&sock_path, "test").await;

    // Cancel the server — this triggers broadcast_shutdown(Manual)
    cancel.cancel();

    // First message should be ShuttingDown
    let msg1: ServerMessage =
        tokio::time::timeout(Duration::from_secs(5), read_message(&mut reader))
            .await
            .expect("should receive ShuttingDown")
            .unwrap();
    match msg1 {
        ServerMessage::ShuttingDown { reason, delay_ms } => {
            assert_eq!(
                reason,
                super::super::protocol::ShutdownReason::Manual,
                "Reason should be Manual"
            );
            assert_eq!(delay_ms, 0, "delay_ms should be 0 (immediate shutdown)");
        }
        other => panic!("Expected ShuttingDown, got {:?}", other),
    }

    // Second message should be Shutdown (after the grace period)
    let msg2: ServerMessage =
        tokio::time::timeout(Duration::from_secs(5), read_message(&mut reader))
            .await
            .expect("should receive Shutdown")
            .unwrap();
    assert!(
        matches!(msg2, ServerMessage::Shutdown),
        "Expected Shutdown, got {:?}",
        msg2
    );
}

#[tokio::test]
async fn agent_busy_set_when_request_forwarded() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("busy_test.sock");
    let mut handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Initially not busy
    assert!(
        !handle.agent_busy.load(Ordering::Relaxed),
        "agent_busy should be false initially"
    );

    // Connect a client and send a request
    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send a request (has method + id)
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test/ping","id":1}"#.into(),
        },
    )
    .await
    .unwrap();

    // Read it from the server side so we know it's been processed
    let forwarded = handle.acp_rx.recv().await.unwrap();
    assert!(forwarded.contains("test/ping"));

    // Now agent_busy should be true
    assert!(
        handle.agent_busy.load(Ordering::Relaxed),
        "agent_busy should be true after forwarding a request"
    );

    handle.cancel.cancel();
}

#[tokio::test]
async fn agent_busy_cleared_when_response_received() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("busy_clear.sock");
    let mut handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and register
    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send a request
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test/ping","id":42}"#.into(),
        },
    )
    .await
    .unwrap();

    // Read the forwarded request and extract the namespaced ID
    let forwarded = handle.acp_rx.recv().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&forwarded).unwrap();
    let namespaced_id = json["id"].as_str().unwrap().to_string();

    assert!(handle.agent_busy.load(Ordering::Relaxed));

    // Send a response back with the namespaced ID
    let response = format!(
        r#"{{"jsonrpc":"2.0","result":{{"ok":true}},"id":"{}"}}"#,
        namespaced_id
    );
    handle.response_tx.send(response).unwrap();

    // Read the response on the client side
    let client_resp: ServerMessage = read_message(&mut reader).await.unwrap();
    assert!(matches!(client_resp, ServerMessage::Acp { .. }));

    // agent_busy should now be false
    // Give a small window for the server loop to process
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !handle.agent_busy.load(Ordering::Relaxed),
        "agent_busy should be false after response is routed"
    );

    handle.cancel.cancel();
}

#[tokio::test]
async fn agent_busy_tracks_multiple_pending_requests() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("busy_multi.sock");
    let mut handle = spawn_leader_server(sock_path.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and register
    let stream = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();

    // Send two requests
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test/a","id":1}"#.into(),
        },
    )
    .await
    .unwrap();
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"test/b","id":2}"#.into(),
        },
    )
    .await
    .unwrap();

    // Read both forwarded requests
    let fwd1 = handle.acp_rx.recv().await.unwrap();
    let fwd2 = handle.acp_rx.recv().await.unwrap();
    let id1 = serde_json::from_str::<serde_json::Value>(&fwd1).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let id2 = serde_json::from_str::<serde_json::Value>(&fwd2).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(handle.agent_busy.load(Ordering::Relaxed));

    // Respond to first request — still busy (one pending)
    handle
        .response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","result":{{}},"id":"{}"}}"#,
            id1
        ))
        .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        handle.agent_busy.load(Ordering::Relaxed),
        "agent_busy should still be true with one request pending"
    );

    // Respond to second request — now idle
    handle
        .response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","result":{{}},"id":"{}"}}"#,
            id2
        ))
        .unwrap();
    let _: ServerMessage = read_message(&mut reader).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !handle.agent_busy.load(Ordering::Relaxed),
        "agent_busy should be false after all responses received"
    );

    handle.cancel.cancel();
}

#[tokio::test]
async fn agent_busy_clears_when_client_disconnects_mid_request() {
    // Verify that agent_busy correctly clears even when the originating
    // client has disconnected before the response arrives. The server
    // should still decrement pending_requests when routing the response,
    // even though the client is gone.
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("busy_disconnect.sock");

    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let client_count = Arc::new(AtomicUsize::new(0));
    let agent_busy = Arc::new(AtomicBool::new(false));
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = cancel.clone();
    let count_clone = client_count.clone();
    let busy_clone = agent_busy.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true, // no_exit_on_disconnect
            count_clone,
            busy_clone,
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect, register, send a request, then disconnect
    let namespaced_id = {
        let stream = LeaderStream::connect(&sock_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);
        write_message(
            &mut writer,
            &ClientMessage::Register {
                client_type: "test".into(),
                mode: ClientMode::Stdio,
                capabilities: ClientCapabilities::default(),
            },
        )
        .await
        .unwrap();
        let _: ServerMessage = read_message(&mut reader).await.unwrap();

        // Send a request
        write_message(
            &mut writer,
            &ClientMessage::Acp {
                payload: r#"{"jsonrpc":"2.0","method":"test/slow","id":1}"#.into(),
            },
        )
        .await
        .unwrap();

        // Read the forwarded request to get the namespaced ID
        let forwarded = acp_rx.recv().await.unwrap();
        let json: serde_json::Value = serde_json::from_str(&forwarded).unwrap();
        let id = json["id"].as_str().unwrap().to_string();

        assert!(
            agent_busy.load(Ordering::Relaxed),
            "should be busy after request"
        );

        // Disconnect the client (stream is dropped here)
        write_message(&mut writer, &ClientMessage::Disconnect)
            .await
            .unwrap();

        id
    };

    // Wait for disconnect to be processed
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Agent_busy should still be true — the request is still in-flight
    assert!(
        agent_busy.load(Ordering::Relaxed),
        "agent_busy should still be true after client disconnect (request still pending)"
    );

    // Now the "agent" sends back a response for the disconnected client's request
    response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","result":{{"done":true}},"id":"{}"}}"#,
            namespaced_id
        ))
        .unwrap();

    // Wait for the server to process it
    tokio::time::sleep(Duration::from_millis(50)).await;

    // agent_busy should now be false — the response was processed and the
    // counter decremented even though the client is gone
    assert!(
        !agent_busy.load(Ordering::Relaxed),
        "agent_busy should be false after response arrives (even though client disconnected)"
    );

    cancel.cancel();
}

/// Regression: bounded(256) client channel + try_send silently dropped
/// notifications during session replay bursts. Unbounded channel fixes this.
#[tokio::test]
async fn high_throughput_replay_no_drops() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader, mut writer) = connect_and_register(&sock_path, "echo-agent-tui").await;

    let load_req =
        r#"{"jsonrpc":"2.0","method":"session/load","id":1,"params":{"session_id":"sess_replay"}}"#;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: load_req.into(),
        },
    )
    .await
    .unwrap();
    // Complete the load so the client leaves the buffering window, then drain
    // the load response so the count below only tallies the notifications.
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    const REPLAY_COUNT: usize = 500;
    for i in 0..REPLAY_COUNT {
        let notification = format!(
            r#"{{"jsonrpc":"2.0","method":"session/notification","params":{{"session_id":"sess_replay","updates":[{{"type":"message_start","message_id":"msg_{i}"}}]}}}}"#,
        );
        response_tx.send(notification).unwrap();
    }

    let mut received = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline - tokio::time::Instant::now();
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, read_message::<_, ServerMessage>(&mut reader)).await {
            Ok(Ok(ServerMessage::Acp { .. })) => {
                received += 1;
                if received == REPLAY_COUNT {
                    break;
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    assert_eq!(
        received, REPLAY_COUNT,
        "All {REPLAY_COUNT} replay notifications must arrive, got {received}"
    );

    cancel.cancel();
}

/// When a client disconnects after interacting with a session, the server
/// sends an `echo.agent/internal/evict_sessions` notification through acp_tx
/// so the agent can release session memory.
#[tokio::test]
async fn evict_sessions_notification_on_disconnect() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let (_response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect, register, and interact with a session so session_owners is populated.
    let (mut _reader, mut writer) = connect_and_register(&sock_path, "test-client").await;

    let msg = r#"{"jsonrpc":"2.0","method":"session/load","id":1,"params":{"sessionId":"sess-evict-test"}}"#;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: msg.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Drain the forwarded ACP message from the channel.
    let _ = acp_rx.recv().await;

    // Disconnect the client.
    write_message(&mut writer, &ClientMessage::Disconnect)
        .await
        .unwrap();
    drop(_reader);
    drop(writer);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The server should have sent an eviction notification through acp_tx.
    let eviction_msg = tokio::time::timeout(Duration::from_secs(1), acp_rx.recv())
        .await
        .expect("should receive eviction notification")
        .expect("channel should not be closed");

    let json: serde_json::Value =
        serde_json::from_str(&eviction_msg).expect("should be valid JSON");
    assert_eq!(
        json["method"].as_str().and_then(|m| m.strip_prefix('_')),
        Some(InternalMethod::EvictSessions.name()),
    );
    let session_ids = json["params"]["sessionIds"]
        .as_array()
        .expect("sessionIds should be an array");
    assert!(
        session_ids
            .iter()
            .any(|v| v.as_str() == Some("sess-evict-test")),
        "eviction should include the session we interacted with, got: {session_ids:?}"
    );

    cancel.cancel();
}

/// When a client disconnects without interacting with any sessions,
/// no eviction notification should be sent.
#[tokio::test]
async fn no_eviction_when_client_has_no_sessions() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let (_response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect, register, but do NOT send any session-related messages.
    let (mut _reader, mut writer) = connect_and_register(&sock_path, "idle-client").await;

    // Disconnect.
    write_message(&mut writer, &ClientMessage::Disconnect)
        .await
        .unwrap();
    drop(_reader);
    drop(writer);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // No eviction should be sent — try_recv should return empty.
    assert!(
        acp_rx.try_recv().is_err(),
        "no eviction notification should be sent for clients with no sessions"
    );

    cancel.cancel();
}

// =========================================================================
// Multi-client broadcast routing
// =========================================================================

/// Read the next `ServerMessage::Acp` payload for a client, ignoring other
/// server messages, with a short deadline. Returns `None` on timeout.
async fn next_acp_payload(reader: &mut tokio::io::ReadHalf<LeaderStream>) -> Option<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    loop {
        let remaining = deadline - tokio::time::Instant::now();
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, read_message::<_, ServerMessage>(reader)).await {
            Ok(Ok(ServerMessage::Acp { payload })) => return Some(payload),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => return None,
        }
    }
}

/// Drain up to a few ACP payloads looking for one containing `needle`.
/// Returns it if found within the window, else `None` (so a "must NOT
/// receive" assertion can use `.is_none()`).
async fn next_acp_payload_matching(
    reader: &mut tokio::io::ReadHalf<LeaderStream>,
    needle: &str,
) -> Option<String> {
    for _ in 0..8 {
        match next_acp_payload(reader).await {
            Some(p) if p.contains(needle) => return Some(p),
            Some(_) => continue,
            None => return None,
        }
    }
    None
}

async fn load_session(writer: &mut tokio::io::WriteHalf<LeaderStream>, session_id: &str) {
    let msg = format!(
        r#"{{"jsonrpc":"2.0","method":"session/load","id":1,"params":{{"sessionId":"{session_id}"}}}}"#
    );
    write_message(writer, &ClientMessage::Acp { payload: msg })
        .await
        .unwrap();
}

/// Regression (live-before-replay race): a live `session/notification` that
/// arrives WHILE a viewer's `session/load` is in flight must be BUFFERED —
/// not delivered early (which would bump the client's eventId highwater and
/// make the subsequent lower-eventId replay get deduped away) — and then
/// flushed, in order, AFTER the load response.
#[tokio::test]
async fn live_broadcast_during_load_is_buffered_then_flushed_after_response() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader, mut writer) = connect_and_register(&sock_path, "viewer").await;

    // Begin a load but do NOT respond yet — the client is mid-load. Read the
    // forwarded request to learn its namespaced id for the response later.
    load_session(&mut writer, "sess-buf").await;
    let forwarded = tokio::time::timeout(Duration::from_secs(1), acp_rx.recv())
        .await
        .expect("timed out waiting for forwarded load")
        .expect("agent channel closed");
    let load_id = serde_json::from_str::<serde_json::Value>(&forwarded)
        .unwrap()
        .get("id")
        .cloned()
        .unwrap();

    // A live broadcast arrives during the load window.
    let live = r#"{"jsonrpc":"2.0","method":"session/notification","params":{"sessionId":"sess-buf","updates":[{"type":"message_start","message_id":"live1"}]}}"#;
    response_tx.send(live.to_string()).unwrap();

    // It must be buffered: the client receives nothing before its response.
    let early = tokio::time::timeout(
        Duration::from_millis(250),
        read_message::<_, ServerMessage>(&mut reader),
    )
    .await;
    assert!(
        early.is_err(),
        "live broadcast must be buffered until the load response, got {early:?}"
    );

    // Send the load response — it flushes the buffered live notification.
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": load_id,
        "result": { "models": [] },
    });
    response_tx.send(response.to_string()).unwrap();

    // Order must be: load response first, then the buffered live notif.
    let first = next_acp_payload(&mut reader).await;
    assert!(
        first.as_deref().is_some_and(|p| p.contains("\"models\"")),
        "first message after load must be the load response, got {first:?}"
    );
    let second = next_acp_payload(&mut reader).await;
    assert!(
        second.as_deref().is_some_and(|p| p.contains("live1")),
        "buffered live notif must arrive (in order) after the load response, got {second:?}"
    );

    cancel.cancel();
}

/// Two clients load the same session; a `session/notification` (no `id`)
/// must reach BOTH (broadcast), while a reverse-request (`id` + `method`)
/// reaches ONLY the driver. The second client's `session/load` must not
/// black out the first (join-not-steal).
#[tokio::test]
async fn two_clients_one_session_broadcast_and_driver() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // Client A loads first → becomes driver. Complete the load (echo a load
    // response) so A leaves the buffering window and receives live broadcasts.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-multi").await;
    complete_load(&mut acp_rx, &response_tx).await;
    // Drain A's load response so the broadcast assertion below reads the notif.
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Client B loads second → joins as subscriber (does not steal).
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-multi").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // A plain notification fans out to both subscribers.
    let notif = r#"{"jsonrpc":"2.0","method":"session/notification","params":{"sessionId":"sess-multi","updates":[{"type":"message_start","message_id":"m1"}]}}"#;
    response_tx.send(notif.to_string()).unwrap();

    let got_a = next_acp_payload(&mut reader_a).await;
    let got_b = next_acp_payload(&mut reader_b).await;
    assert!(
        got_a.as_deref().is_some_and(|p| p.contains("m1")),
        "client A must receive the broadcast notification, got {got_a:?}"
    );
    assert!(
        got_b.as_deref().is_some_and(|p| p.contains("m1")),
        "client B must receive the broadcast notification (no blackout), got {got_b:?}"
    );

    // A NON-interaction reverse-request (has both id + method) goes to the
    // driver (A) only. (Interaction reverse-requests are shared — see
    // `interaction_request_broadcasts_to_all_subscribers`.)
    let req = r#"{"jsonrpc":"2.0","id":42,"method":"fs/read_text_file","params":{"sessionId":"sess-multi","path":"/tmp/x"}}"#;
    response_tx.send(req.to_string()).unwrap();

    let req_a = next_acp_payload(&mut reader_a).await;
    let req_b = next_acp_payload(&mut reader_b).await;
    assert!(
        req_a
            .as_deref()
            .is_some_and(|p| p.contains("read_text_file")),
        "driver A must receive the reverse-request, got {req_a:?}"
    );
    assert!(
        req_b.is_none(),
        "non-driver B must NOT receive the reverse-request, got {req_b:?}"
    );

    cancel.cancel();
}

/// A `echo.agent/scheduled_task_inject_prompt` (cron `/loop` fire) must be routed
/// to the SINGLE session driver, not fanned out to every subscriber. If it
/// broadcast, each attached dashboard would enqueue + try to drive the same
/// cron turn (phantom `#N` queue rows, competing drivers, stuck turns). The
/// other clients render the resulting turn from the broadcast deltas.
#[tokio::test]
async fn scheduled_task_inject_prompt_routes_to_driver_only() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // Client A loads first → becomes driver.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-cron").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Client B loads second → joins as subscriber (does not steal driver).
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-cron").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // The agent fires a scheduled task → inject_prompt notification, in the
    // real gateway-WRAPPED wire form (`_echo.agent/...` top-level, nested method +
    // params) — the shape that previously fell through to broadcast.
    let inject = r#"{"method":"_echo.agent/scheduled_task_inject_prompt","params":{"method":"echo.agent/scheduled_task_inject_prompt","params":{"sessionId":"sess-cron","taskId":"task-1","prompt":"echo hello","humanSchedule":"every 1m"}}}"#;
    response_tx.send(inject.to_string()).unwrap();

    let got_a = next_acp_payload(&mut reader_a).await;
    let got_b = next_acp_payload(&mut reader_b).await;
    assert!(
        got_a
            .as_deref()
            .is_some_and(|p| p.contains("scheduled_task_inject_prompt")),
        "driver A must receive the cron inject_prompt, got {got_a:?}"
    );
    assert!(
        got_b.is_none(),
        "non-driver B must NOT receive the cron inject_prompt, got {got_b:?}"
    );

    cancel.cancel();
}

/// A blocking interaction reverse-request (permission / `ask_user_question` /
/// plan-approval) is SHARED: broadcast to every subscriber so any client can
/// render + answer the modal. Contrast
/// with `two_clients_one_session_broadcast_and_driver`, where an ordinary
/// reverse-request reaches the driver only.
#[tokio::test]
async fn interaction_request_broadcasts_to_all_subscribers() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // The agent raises an `ask_user_question` reverse-request in the real
    // gateway-WRAPPED wire form (`_echo.agent/...` top-level, method+params nested).
    // This is the shape that previously fell through to driver-only.
    let req = r#"{"jsonrpc":"2.0","id":501,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"sess-int","toolCallId":"tc-q","questions":[]}}}"#;
    response_tx.send(req.to_string()).unwrap();

    let got_a = next_acp_payload_matching(&mut reader_a, "ask_user_question").await;
    let got_b = next_acp_payload_matching(&mut reader_b, "ask_user_question").await;
    assert!(
        got_a.is_some(),
        "driver A must receive the shared interaction"
    );
    assert!(
        got_b.is_some(),
        "subscriber B must ALSO receive the shared interaction (not driver-only)"
    );

    cancel.cancel();
}

/// A client that attaches WHILE an interaction is pending must render it too:
/// the leader caches the issued interaction and replays it to the new
/// subscriber after its `session/load` completes.
#[tokio::test]
async fn pending_interaction_replayed_to_late_joiner() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Interaction raised (wrapped wire form) while only A is attached →
    // cached by the leader.
    let req = r#"{"jsonrpc":"2.0","id":601,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"sess-int","toolCallId":"tc-late","questions":[]}}}"#;
    response_tx.send(req.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "ask_user_question").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // B attaches LATE → must receive the replayed cached interaction.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let replayed = next_acp_payload_matching(&mut reader_b, "ask_user_question").await;
    assert!(
        replayed.is_some(),
        "a late-joiner must receive the replayed still-pending interaction"
    );

    cancel.cancel();
}

/// Like [`connect_and_register`] but also returns the server-assigned
/// `ClientId` (needed to address targeted replay payloads at the client).
async fn connect_register_get_id(
    sock_path: &std::path::Path,
    client_type: &str,
) -> (
    tokio::io::ReadHalf<LeaderStream>,
    tokio::io::WriteHalf<LeaderStream>,
    ClientId,
) {
    let stream = LeaderStream::connect(sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: client_type.into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let msg: ServerMessage = read_message(&mut reader).await.unwrap();
    let ServerMessage::Registered { client_id, .. } = msg else {
        panic!("expected Registered, got {msg:?}");
    };
    (reader, writer, ClientId(client_id))
}

/// A client that reattaches AFTER a subagent spawned is backfilled into
/// the child route when its parent `session/load` response lands: the
/// parent→child index survives the disconnect eviction (which only
/// empties subscriber sets), so live child updates resume without any
/// replayed spawn line. Driver inheritance is pinned too: a driver-only
/// child reverse-request must reach the reattached client.
#[tokio::test]
async fn reattached_client_backfilled_into_child_routes() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // A attaches and a subagent spawns LIVE → child route = snapshot {A},
    // index edge sess-sub → child-sub.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-sub").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-sub","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-sub"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "subagent_spawned")
            .await
            .is_some(),
        "sanity: A receives the live spawn"
    );

    // A disconnects: subscriber sets empty + evicted; the index edge stays.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A2 reconnects and reloads the parent — no replayed spawn line is
    // sent, so receiving child updates proves the response-time BACKFILL
    // (not replay registration).
    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-sub").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-sub","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"CHILD_LIVE_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "CHILD_LIVE_DELTA")
            .await
            .is_some(),
        "live child updates must reach the reattached client via backfill"
    );

    // Backfill also re-seeded the child driver (parent driver = A2), so a
    // driver-only child reverse-request routes to A2 instead of dropping.
    let child_reverse = r#"{"jsonrpc":"2.0","id":777,"method":"echo.agent/child_thing","params":{"sessionId":"child-sub"}}"#;
    response_tx.send(child_reverse.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "child_thing")
            .await
            .is_some(),
        "child reverse-requests must reach the backfilled driver"
    );

    cancel.cancel();
}

/// A loading client receives the child route from the targeted REPLAYED
/// `subagent_spawned` alone (fresh-leader relaunch: no live spawn ever
/// crossed this server instance, the index is empty, only replay lines
/// describe the subagent).
#[tokio::test]
async fn replayed_spawn_registers_child_route_for_loading_client() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a, a_id) = connect_register_get_id(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-fresh").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-fresh","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_spawned","child_session_id":"child-fresh"}}}}}}"#,
        a_id.0
    );
    response_tx.send(spawned_replay).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "subagent_spawned")
            .await
            .is_some(),
        "the replayed spawn row reaches the loading client"
    );

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-fresh","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"CHILD_FRESH_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "CHILD_FRESH_DELTA")
            .await
            .is_some(),
        "the replayed spawn must register the live child route"
    );

    cancel.cancel();
}

/// A client that attaches to the parent while another client already holds
/// a live child route is backfilled into that route (child sets are
/// spawn-time snapshots; joining the parent must join its descendants).
#[tokio::test]
async fn late_attacher_backfilled_into_existing_child_routes() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-sub2").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Live spawn while only A subscribes → child route = snapshot {A}.
    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-sub2","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-sub2"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    // B attaches AFTER the spawn.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-sub2").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-sub2","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"CHILD_LIVE_DELTA2"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "CHILD_LIVE_DELTA2")
            .await
            .is_some(),
        "A (in the spawn-time snapshot) still receives child updates"
    );
    assert!(
        next_acp_payload_matching(&mut reader_b, "CHILD_LIVE_DELTA2")
            .await
            .is_some(),
        "the late attacher must be backfilled into the child route"
    );

    cancel.cancel();
}

/// A replayed `subagent_finished` unsubscribes ONLY its target client:
/// another client's live child route must survive one client's history
/// replay (full teardown is reserved for the LIVE finish).
#[tokio::test]
async fn replayed_finished_does_not_tear_down_live_child_route() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // A holds the live child route.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-tear").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-tear","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-tear"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    // B attaches (backfilled into the child route), then its replay
    // contains a finished line for the child → B alone is unsubscribed.
    let (mut reader_b, mut writer_b, b_id) = connect_register_get_id(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-tear").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let finished_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-tear","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_finished","child_session_id":"child-tear"}}}}}}"#,
        b_id.0
    );
    response_tx.send(finished_replay).unwrap();
    let _ = next_acp_payload_matching(&mut reader_b, "subagent_finished").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-tear","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"CHILD_TEAR_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "CHILD_TEAR_DELTA")
            .await
            .is_some(),
        "A's live child route must survive B's replayed finished"
    );
    assert!(
        next_acp_payload_matching(&mut reader_b, "CHILD_TEAR_DELTA")
            .await
            .is_none(),
        "B was unsubscribed by ITS replayed finished"
    );

    cancel.cancel();
}

/// Backfill walks the index depth-first: a nested child (spawned under a
/// CHILD session) is also joined when a client attaches to the root
/// parent.
#[tokio::test]
async fn backfill_covers_nested_children() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-nest").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // child under parent, grandchild under child (live broadcasts).
    let spawned_child = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-nest","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-nest"}}}"#;
    response_tx.send(spawned_child.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;
    let spawned_grandchild = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"child-nest","update":{"sessionUpdate":"subagent_spawned","child_session_id":"grandchild-nest"}}}"#;
    response_tx.send(spawned_grandchild.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "grandchild-nest").await;

    // A reattaches: backfill must cover BOTH levels.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-nest").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let grandchild_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"grandchild-nest","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"GRANDCHILD_DELTA"}}}}"#;
    response_tx.send(grandchild_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "GRANDCHILD_DELTA")
            .await
            .is_some(),
        "backfill must subscribe the client to nested descendants"
    );

    cancel.cancel();
}

/// Re-parenting on an INTERMEDIATE finish: root → A → B (both live). A
/// finishes LIVE while B keeps running. A new client loading the ROOT must
/// still be backfilled into B's live route — `prune_child_route` promotes B
/// onto A's parent so the forward-only root walk reaches it. Without
/// re-parenting the root→A edge is gone and B's subtree is orphaned.
#[tokio::test]
async fn intermediate_finish_reparents_live_grandchild_for_root_backfill() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-rep").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // root → A → B (live spawns).
    let spawned_a = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-rep","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-a"}}}"#;
    response_tx.send(spawned_a.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "child-a").await;
    let spawned_b = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"child-a","update":{"sessionUpdate":"subagent_spawned","child_session_id":"grandchild-b"}}}"#;
    response_tx.send(spawned_b.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "grandchild-b").await;

    // Intermediate A finishes LIVE; B keeps running.
    let finished_a = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-rep","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-a"}}}"#;
    response_tx.send(finished_a.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_finished").await;

    // A new client loads the ROOT.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-rep").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // B is still live: its delta must reach the reattached client.
    let grandchild_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"grandchild-b","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"LIVE_GRANDCHILD_AFTER_A_FINISH"}}}}"#;
    response_tx.send(grandchild_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "LIVE_GRANDCHILD_AFTER_A_FINISH")
            .await
            .is_some(),
        "an intermediate finish must re-parent the live grandchild so root backfill still reaches it"
    );

    cancel.cancel();
}

/// The LIVE `subagent_finished` still tears the route down globally and
/// prunes the index: after it, a reattaching client is NOT backfilled
/// into the dead child (no leaked routes for finished subagents).
#[tokio::test]
async fn live_finished_prunes_index_so_reattach_skips_dead_child() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-dead").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-dead","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-dead"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;
    let finished_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-dead","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-dead"}}}"#;
    response_tx.send(finished_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_finished").await;

    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-dead").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // A stray update on the dead child must not reach A2 (it is
    // relay-classified — the route and index entry are gone).
    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-dead","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"DEAD_CHILD_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "DEAD_CHILD_DELTA")
            .await
            .is_none(),
        "a finished child's route must not be resurrected by reattach"
    );

    cancel.cancel();
}

/// Symmetric twin of `live_finished_prunes_index_so_reattach_skips_dead_child`
/// for the no-subscribers case: the parent goes fully detached (every
/// client disconnects, the index edge survives), THEN a live
/// `subagent_finished` arrives. It is relay-classified (no subscribers) and
/// dropped — but it must still prune the index edge, so a reattaching
/// client's `session/load` backfill does not resurrect the dead child.
#[tokio::test]
async fn detached_live_finished_prunes_index_so_reattach_skips_dead_child() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-detach").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-detach","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-detach"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    // Every client disconnects → parent fully detached. Eviction empties
    // the subscriber/driver maps but keeps the `child_sessions` edge.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Live finish arrives while detached: relay-classified (no subscribers)
    // and dropped, but it must still prune the dead child's edge.
    let finished_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-detach","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-detach"}}}"#;
    response_tx.send(finished_live.to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-detach").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-detach","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"DETACHED_DEAD_CHILD_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "DETACHED_DEAD_CHILD_DELTA")
            .await
            .is_none(),
        "a detached live finish must prune the edge — the dead child's route \
         must not be resurrected by reattach backfill"
    );

    cancel.cancel();
}

/// A loader disconnecting between a dead child's replayed spawn and
/// replayed finish must not leak the index edge: the orphan-drop arm still
/// prunes when nothing holds the route, so a later attacher is not
/// backfilled into the dead child.
#[tokio::test]
async fn mid_burst_disconnect_still_prunes_dead_child_route() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a, a_id) = connect_register_get_id(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-leak").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-leak","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_spawned","child_session_id":"child-leak"}}}}}}"#,
        a_id.0
    );
    response_tx.send(spawned_replay).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    // Disconnect mid-burst: eviction empties the routes, the index edge
    // stays. The dead child's replayed finish then arrives orphaned.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let finished_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-leak","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_finished","child_session_id":"child-leak"}}}}}}"#,
        a_id.0
    );
    response_tx.send(finished_replay).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let (mut reader_a2, mut writer_a2) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a2, "sess-leak").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-leak","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"LEAKED_CHILD_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a2, "LEAKED_CHILD_DELTA")
            .await
            .is_none(),
        "an orphaned replayed finish must prune the edge — the dead child's \
         route must not be resurrected by reattach backfill"
    );

    cancel.cancel();
}

/// An ORPHANED replayed finish (its target already vanished) must leave a
/// route other clients hold untouched — the orphan branch prunes only
/// when nothing holds the route. An always-prune mutation of that guard
/// would let one dead client's stale replay burst tear down A's live
/// route.
#[tokio::test]
async fn orphaned_replayed_finished_leaves_held_route_untouched() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // A holds the live child route.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-hold").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-hold","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-hold"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    // B registers and vanishes; a replayed finish targeted at B arrives
    // orphaned while A still holds the route.
    let (reader_b, writer_b, b_id) = connect_register_get_id(&sock_path, "client-b").await;
    drop(reader_b);
    drop(writer_b);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let finished_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-hold","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_finished","child_session_id":"child-hold"}}}}}}"#,
        b_id.0
    );
    response_tx.send(finished_replay).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-hold","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"HELD_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "HELD_DELTA")
            .await
            .is_some(),
        "an orphaned replayed finish must not prune a route A still holds"
    );

    cancel.cancel();
}

/// A replayed spawn UNIONS the loading client into an existing live route:
/// a regression to the live arm's snapshot-replace would tear down the
/// holder's route on someone else's history replay (the symmetric twin of
/// `replayed_finished_does_not_tear_down_live_child_route`).
#[tokio::test]
async fn replayed_spawn_unions_into_existing_live_route() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-union").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-union","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-union"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;

    let (mut reader_b, mut writer_b, b_id) = connect_register_get_id(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-union").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-union","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_spawned","child_session_id":"child-union"}}}}}}"#,
        b_id.0
    );
    response_tx.send(spawned_replay).unwrap();
    let _ = next_acp_payload_matching(&mut reader_b, "subagent_spawned").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-union","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"UNION_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_a, "UNION_DELTA")
            .await
            .is_some(),
        "A's live route must survive B's replayed spawn (union, not replace)"
    );
    assert!(
        next_acp_payload_matching(&mut reader_b, "UNION_DELTA")
            .await
            .is_some(),
        "B is in the route too"
    );

    cancel.cancel();
}

/// A replayed finish that removes the LAST subscriber prunes the route,
/// driver, and index edge — a later attacher must not be backfilled into
/// a child whose finish was only ever observed via replay.
#[tokio::test]
async fn replayed_finished_last_subscriber_prunes_dead_child() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // Fresh leader: A's replay is the only subscription path.
    let (mut reader_a, mut writer_a, a_id) = connect_register_get_id(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-last").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let spawned_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-last","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_spawned","child_session_id":"child-last"}}}}}}"#,
        a_id.0
    );
    response_tx.send(spawned_replay).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;
    let finished_replay = format!(
        r#"{{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{{"sessionId":"sess-last","_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}},"update":{{"sessionUpdate":"subagent_finished","child_session_id":"child-last"}}}}}}"#,
        a_id.0
    );
    response_tx.send(finished_replay).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_finished").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // B attaches: backfill must find no edge for the dead child.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-last").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-last","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"LAST_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    assert!(
        next_acp_payload_matching(&mut reader_b, "LAST_DELTA")
            .await
            .is_none(),
        "the last-subscriber replayed finish must prune the edge"
    );
    assert!(
        next_acp_payload_matching(&mut reader_a, "LAST_DELTA")
            .await
            .is_none(),
        "A was unsubscribed by its own replayed finish"
    );

    cancel.cancel();
}

/// Isolates the REQUEST-side backfill call site: it subscribes the loader
/// to live children the moment the `session/load` request passes through,
/// so a child delta arriving MID-LOAD (post-request, pre-response) is
/// delivered instead of dropped as subscriber-less. With only the
/// response-side site the delta would be lost before the response lands.
#[tokio::test]
async fn mid_load_child_delta_reaches_loader_via_request_side_backfill() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // Seed the index: A spawns the child live, then leaves.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-midload").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let spawned_live = r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-midload","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-midload"}}}"#;
    response_tx.send(spawned_live.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "subagent_spawned").await;
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // B begins a load but the agent has NOT responded yet; a child delta
    // lands in that window.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-midload").await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let child_live = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"child-midload","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"MIDLOAD_DELTA"}}}}"#;
    response_tx.send(child_live.to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    complete_load(&mut acp_rx, &response_tx).await;
    assert!(
        next_acp_payload_matching(&mut reader_b, "MIDLOAD_DELTA")
            .await
            .is_some(),
        "a mid-load child delta must reach the loader (request-side backfill)"
    );

    cancel.cancel();
}

/// A pending interaction must SURVIVE a full client disconnect and be
/// replayed on reconnect. A session with a pending interaction has a running
/// turn (the tool awaits the answer), so the agent keeps it resident across
/// the disconnect with the reverse-request still parked
/// (`session_has_live_work`). The leader must therefore NOT drop its
/// interaction cache on detach — otherwise the reconnecting client gets no
/// modal while the agent is still waiting. Regression for the "modal vanishes
/// on reconnect" bug.
#[tokio::test]
async fn pending_interaction_survives_disconnect_and_replays_on_reconnect() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // A attaches; interaction raised while only A is attached → cached.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let req = r#"{"jsonrpc":"2.0","id":801,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"sess-int","toolCallId":"tc-reconnect","questions":[]}}}"#;
    response_tx.send(req.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "ask_user_question").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // A FULLY disconnects (it is the only subscriber). Previously this dropped
    // the interaction cache for the session.
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Reconnect (B) → the still-pending interaction must replay on its load.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let replayed = next_acp_payload_matching(&mut reader_b, "ask_user_question").await;
    assert!(
        replayed.is_some(),
        "a still-pending interaction must survive a full disconnect and replay on reconnect"
    );

    cancel.cancel();
}

/// An interaction raised while the session has NO subscriber (a session
/// started from the dashboard whose turn hit `ask_user_question` before
/// anyone entered it, or a reverse-request that races ahead of the
/// `session/new`/`session/load` response that registers the subscriber) must
/// still be cached, so the FIRST client to attach gets the modal replayed.
/// Regression for the "entered the session, modal never appears, turn stuck
/// Waiting" bug — the cache insert used to be gated on an existing subscriber.
#[tokio::test]
async fn interaction_raised_with_no_subscriber_is_cached_and_replayed_on_first_attach() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    // Interaction raised BEFORE any client attaches/subscribes.
    let req = r#"{"jsonrpc":"2.0","id":901,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"sess-int","toolCallId":"tc-nosub","questions":[]}}}"#;
    response_tx.send(req.to_string()).unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;

    // First client attaches → must receive the replayed cached interaction.
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let replayed = next_acp_payload_matching(&mut reader_a, "ask_user_question").await;
    assert!(
        replayed.is_some(),
        "an interaction raised with no subscriber must be cached and replayed to the first client that attaches"
    );

    cancel.cancel();
}

/// Once an interaction resolves (first-answer-wins → `InteractionResolved`),
/// the leader evicts it from the replay cache, so a client that attaches
/// afterwards does NOT get a stale modal.
#[tokio::test]
async fn resolved_interaction_not_replayed_to_late_joiner() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx, mut acp_rx) =
        setup_persistent_server_with_agent(&temp).await;

    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let _ = next_acp_payload(&mut reader_a).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Raise then resolve the interaction (wrapped wire form), no other client
    // attached yet.
    let req = r#"{"jsonrpc":"2.0","id":701,"method":"_echo.agent/ask_user_question","params":{"method":"echo.agent/ask_user_question","params":{"sessionId":"sess-int","toolCallId":"tc-ev","questions":[]}}}"#;
    response_tx.send(req.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "ask_user_question").await;

    let resolved = r#"{"method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"sess-int","update":{"sessionUpdate":"interaction_resolved","tool_call_id":"tc-ev"}}}}"#;
    response_tx.send(resolved.to_string()).unwrap();
    let _ = next_acp_payload_matching(&mut reader_a, "interaction_resolved").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    // B attaches AFTER resolution → must NOT receive the evicted interaction.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-int").await;
    complete_load(&mut acp_rx, &response_tx).await;
    let replayed = next_acp_payload_matching(&mut reader_b, "ask_user_question").await;
    assert!(
        replayed.is_none(),
        "a resolved interaction must NOT be replayed to a late-joiner (evicted)"
    );

    cancel.cancel();
}

/// When the driver disconnects but another subscriber remains, the session
/// is NOT evicted and the driver role transfers to the remaining client.
#[tokio::test]
async fn driver_disconnect_transfers_not_evicts() {
    let temp = TempDir::new().unwrap();
    let sock_path = temp.path().join("test.sock");
    let (acp_tx, mut acp_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let control_state = default_test_control_state(&sock_path);

    let sock_clone = sock_path.clone();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        let _ = run_leader_server(
            sock_clone,
            acp_tx,
            response_rx,
            cancel_clone,
            true,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            AgentActivity::default(),
            watch::channel(true).1,
            watch::channel(false).0,
            watch::channel(super::super::protocol::ShutdownReason::Manual).0,
            None,
            control_state,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A is driver, B is subscriber.
    let (reader_a, mut writer_a) = connect_and_register(&sock_path, "client-a").await;
    load_session(&mut writer_a, "sess-xfer").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "client-b").await;
    load_session(&mut writer_b, "sess-xfer").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Drain the forwarded session/load messages to the agent.
    while acp_rx.try_recv().is_ok() {}

    // Driver A disconnects.
    write_message(&mut writer_a, &ClientMessage::Disconnect)
        .await
        .unwrap();
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(80)).await;

    // No eviction should be sent — B still subscribes.
    assert!(
        acp_rx.try_recv().is_err(),
        "session must NOT be evicted while another subscriber remains"
    );

    // A NON-interaction reverse-request now reaches B (driver transferred).
    // (A non-interaction method exercises the driver-only path; interaction
    // reverse-requests are broadcast instead — see the shared-interaction
    // tests.)
    let req = r#"{"jsonrpc":"2.0","id":7,"method":"fs/read_text_file","params":{"sessionId":"sess-xfer","path":"/tmp/x"}}"#;
    response_tx.send(req.to_string()).unwrap();
    let req_b = next_acp_payload(&mut reader_b).await;
    assert!(
        req_b
            .as_deref()
            .is_some_and(|p| p.contains("read_text_file")),
        "after driver disconnect, B should become driver and receive the reverse-request, got {req_b:?}"
    );

    cancel.cancel();
}

/// `echo.agent/sessions/changed` is a machine-wide roster notification with no
/// sessionId; it must broadcast to every registered client (not just the
/// last-active one) so all open dashboards stay in sync.
#[tokio::test]
async fn roster_changed_broadcasts_to_all_clients() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader_a, _writer_a) = connect_and_register(&sock_path, "client-a").await;
    let (mut reader_b, _writer_b) = connect_and_register(&sock_path, "client-b").await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let changed = r#"{"jsonrpc":"2.0","method":"echo.agent/sessions/changed","params":{"upserted":[{"sessionId":"sess-roster","cwd":"/repo","isWorktree":false,"yolo":false,"activity":"working","resident":true,"lastChangeUnixMs":1,"origin":{"kind":"local"}}],"removed":[]}}"#;
    response_tx.send(changed.to_string()).unwrap();

    let got_a = next_acp_payload(&mut reader_a).await;
    let got_b = next_acp_payload(&mut reader_b).await;
    assert!(
        got_a.as_deref().is_some_and(|p| p.contains("sess-roster")),
        "client A must receive the roster broadcast, got {got_a:?}"
    );
    assert!(
        got_b.as_deref().is_some_and(|p| p.contains("sess-roster")),
        "client B must receive the roster broadcast, got {got_b:?}"
    );

    cancel.cancel();
}

/// `echo.agent/models/update` is a machine-wide catalog notification with no
/// sessionId; it must broadcast to every registered client so every model
/// picker refreshes after a config.toml / models_cache.json hot-reload —
/// not just the last-active client. Uses the production wire form: agent
/// ext notifications arrive `_`-prefixed (`_echo.agent/models/update`).
#[tokio::test]
async fn models_update_broadcasts_to_all_clients() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader_a, _writer_a) = connect_and_register(&sock_path, "client-a").await;
    let (mut reader_b, _writer_b) = connect_and_register(&sock_path, "client-b").await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let update = r#"{"jsonrpc":"2.0","method":"_echo.agent/models/update","params":{"currentModelId":"echo-agent-new","availableModels":[{"modelId":"echo-agent-new","name":"EchoAgent New"}]}}"#;
    response_tx.send(update.to_string()).unwrap();

    let got_a = next_acp_payload(&mut reader_a).await;
    let got_b = next_acp_payload(&mut reader_b).await;
    assert!(
        got_a
            .as_deref()
            .is_some_and(|p| p.contains("echo-agent-new")),
        "client A must receive the models broadcast, got {got_a:?}"
    );
    assert!(
        got_b
            .as_deref()
            .is_some_and(|p| p.contains("echo-agent-new")),
        "client B must receive the models broadcast, got {got_b:?}"
    );

    cancel.cancel();
}

/// `echo.agent/mcp/servers_updated` is a machine-wide MCP-catalog notification
/// with no sessionId (session-agnostic by design); it must broadcast to
/// every registered client so managed connectors don't vanish from clients
/// that weren't last-active when the post-initialize background fetch
/// resolved. Uses the production wire form (`_`-prefixed ext notification
/// with the real method nested in params).
#[tokio::test]
async fn mcp_servers_updated_broadcasts_to_all_clients() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader_a, _writer_a) = connect_and_register(&sock_path, "client-a").await;
    let (mut reader_b, _writer_b) = connect_and_register(&sock_path, "client-b").await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let update = r#"{"jsonrpc":"2.0","method":"_echo.agent/mcp/servers_updated","params":{"method":"echo.agent/mcp/servers_updated","params":{"mcpServers":[{"name":"echo_agent_com_slack","source":"managed"}]}}}"#;
    response_tx.send(update.to_string()).unwrap();

    let got_a = next_acp_payload(&mut reader_a).await;
    let got_b = next_acp_payload(&mut reader_b).await;
    assert!(
        got_a
            .as_deref()
            .is_some_and(|p| p.contains("echo_agent_com_slack")),
        "client A must receive the MCP catalog broadcast, got {got_a:?}"
    );
    assert!(
        got_b
            .as_deref()
            .is_some_and(|p| p.contains("echo_agent_com_slack")),
        "client B must receive the MCP catalog broadcast, got {got_b:?}"
    );

    cancel.cancel();
}

/// The broadcast classifier must accept both wire forms (`_`-prefixed
/// production ext notifications and direct methods) for the machine-wide
/// set, and reject sessionful / unrelated methods.
#[test]
fn machine_wide_broadcast_classifier_matches_both_wire_forms() {
    // Direct forms.
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/sessions/changed","params":{}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/models/update","params":{}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/mcp/servers_updated","params":{}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/announcements/update","params":{}}"#
    )));
    // `_`-prefixed production ext-notification forms.
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"_echo.agent/sessions/changed","params":{}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"_echo.agent/models/update","params":{}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"_echo.agent/mcp/servers_updated","params":{"method":"echo.agent/mcp/servers_updated","params":{"mcpServers":[]}}}"#
    )));
    assert!(is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"_echo.agent/announcements/update","params":{"method":"echo.agent/announcements/update","params":{"gen":2,"announcements":[]}}}"#
    )));
    // Non-broadcast methods. `echo.agent/settings/update` must stay unicast —
    // it carries auth/gate state resolved for the requesting client.
    assert!(!is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s"}}"#
    )));
    assert!(!is_machine_wide_broadcast_notification(&pv(
        r#"{"jsonrpc":"2.0","method":"echo.agent/settings/update","params":{}}"#
    )));
}

// =========================================================================
// Code-nav capability injection tests
// =========================================================================

/// Verify that the leader injects `codeNavEnabled: true` into session/new
/// when the client registered with `code_nav_enabled: true`.
#[test]
fn inject_capabilities_sets_code_nav_enabled_true() {
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: None,
        client_version: None,
        code_nav_enabled: true,
        ..Default::default()
    };
    let payload =
        r#"{"jsonrpc":"2.0","method":"session/new","id":1,"params":{"cwd":"/repo","_meta":{}}}"#;
    let mut json = pv(payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-web", ClientId(1));
    assert_eq!(
        json["params"]["_meta"]["codeNavEnabled"],
        serde_json::json!(true),
        "leader must inject codeNavEnabled=true for code-nav-capable client"
    );
}

/// Verify that the leader injects `codeNavEnabled: false` when the client
/// did NOT register with `code_nav_enabled` — preventing a prior eligible
/// client's shared state from bleeding into this client's sessions.
#[test]
fn inject_capabilities_sets_code_nav_enabled_false() {
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: None,
        client_version: None,
        code_nav_enabled: false,
        ..Default::default()
    };
    let payload = r#"{"jsonrpc":"2.0","method":"session/new","id":1,"params":{"cwd":"/repo","_meta":{"clientIdentifier":"echo-agent-tui"}}}"#;
    let mut json = pv(payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-tui", ClientId(1));
    assert_eq!(
        json["params"]["_meta"]["codeNavEnabled"],
        serde_json::json!(false),
        "leader must inject codeNavEnabled=false for client without code-nav capability"
    );
}

/// Verify that `codeNavEnabled` is also injected into `session/load` so
/// reconnect sessions inherit the correct per-client capability.
#[test]
fn inject_capabilities_injects_code_nav_into_session_load() {
    let caps = ClientCapabilities {
        yolo_mode: false,
        default_model: None,
        client_version: None,
        code_nav_enabled: true,
        ..Default::default()
    };
    let payload = r#"{"jsonrpc":"2.0","method":"session/load","id":2,"params":{"sessionId":"abc","cwd":"/repo","_meta":{}}}"#;
    let mut json = pv(payload);
    inject_session_request_context(&mut json, &caps, "echo-agent-web", ClientId(1));
    assert_eq!(
        json["params"]["_meta"]["codeNavEnabled"],
        serde_json::json!(true),
        "leader must inject codeNavEnabled into session/load for reconnect isolation"
    );
}

/// Verify leader-mode client isolation: two clients with different code-nav
/// capabilities get independent `codeNavEnabled` values injected into their
/// session/new requests.
#[test]
fn inject_capabilities_two_clients_stay_isolated() {
    let web_caps = ClientCapabilities {
        code_nav_enabled: true,
        ..Default::default()
    };
    let tui_caps = ClientCapabilities {
        code_nav_enabled: false,
        ..Default::default()
    };
    let session_new =
        r#"{"jsonrpc":"2.0","method":"session/new","id":1,"params":{"cwd":"/repo","_meta":{}}}"#;

    let mut web_json = pv(session_new);
    inject_session_request_context(&mut web_json, &web_caps, "echo-agent-web", ClientId(1));
    let mut tui_json = pv(session_new);
    inject_session_request_context(&mut tui_json, &tui_caps, "echo-agent-tui", ClientId(2));

    assert_eq!(
        web_json["params"]["_meta"]["codeNavEnabled"],
        serde_json::json!(true)
    );
    assert_eq!(
        tui_json["params"]["_meta"]["codeNavEnabled"],
        serde_json::json!(false)
    );
}

#[test]
fn inject_capabilities_terminal_and_fs_per_client() {
    let web_caps = ClientCapabilities {
        terminal: true,
        fs_read: true,
        fs_write: true,
        ..Default::default()
    };
    let tui_caps = ClientCapabilities {
        terminal: false,
        fs_read: false,
        fs_write: false,
        ..Default::default()
    };
    let session_new =
        r#"{"jsonrpc":"2.0","method":"session/new","id":1,"params":{"cwd":"/repo","_meta":{}}}"#;

    let mut web_json = pv(session_new);
    inject_session_request_context(&mut web_json, &web_caps, "echo-agent-web", ClientId(1));
    let mut tui_json = pv(session_new);
    inject_session_request_context(&mut tui_json, &tui_caps, "echo-agent-tui", ClientId(2));

    assert_eq!(
        web_json["params"]["_meta"]["clientTerminal"],
        serde_json::json!(true)
    );
    assert_eq!(
        web_json["params"]["_meta"]["clientFsRead"],
        serde_json::json!(true)
    );
    assert_eq!(
        web_json["params"]["_meta"]["clientFsWrite"],
        serde_json::json!(true)
    );

    assert_eq!(
        tui_json["params"]["_meta"]["clientTerminal"],
        serde_json::json!(false)
    );
    assert_eq!(
        tui_json["params"]["_meta"]["clientFsRead"],
        serde_json::json!(false)
    );
    assert_eq!(
        tui_json["params"]["_meta"]["clientFsWrite"],
        serde_json::json!(false)
    );
}

#[test]
fn inject_capabilities_terminal_into_session_load() {
    let caps = ClientCapabilities {
        terminal: true,
        fs_read: false,
        fs_write: false,
        ..Default::default()
    };
    let session_load = r#"{"jsonrpc":"2.0","method":"session/load","id":2,"params":{"sessionId":"sess-1","_meta":{}}}"#;

    let mut json = pv(session_load);
    inject_session_request_context(&mut json, &caps, "echo-agent-web", ClientId(1));

    assert_eq!(
        json["params"]["_meta"]["clientTerminal"],
        serde_json::json!(true)
    );
    assert_eq!(
        json["params"]["_meta"]["clientFsRead"],
        serde_json::json!(false)
    );
    assert_eq!(
        json["params"]["_meta"]["clientFsWrite"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn subagent_child_session_routed_after_spawned() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader, mut writer) = connect_and_register(&sock_path, "test").await;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-parent","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Agent sends SubagentSpawned on parent session
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-parent","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-123"}}}"#.into())
        .unwrap();
    let _: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .unwrap()
            .unwrap();

    // Notification on child session should be routed to the parent owner
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"child-123","update":{"sessionUpdate":"message_delta","content":"hello"}}}"#.into())
        .unwrap();

    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .expect("child session notification should reach parent owner")
            .unwrap();
    match msg {
        ServerMessage::Acp { payload } => {
            let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(json["params"]["sessionId"], "child-123");
        }
        other => panic!("Expected Acp, got {:?}", other),
    }

    cancel.cancel();
}

#[tokio::test]
async fn subagent_child_session_cleaned_up_on_finished() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    let (mut reader, mut writer) = connect_and_register(&sock_path, "test").await;
    write_message(
        &mut writer,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-parent","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // SubagentSpawned registers child
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-parent","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-456"}}}"#.into())
        .unwrap();
    let _: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .unwrap()
            .unwrap();

    // SubagentFinished deregisters child
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"sess-parent","update":{"sessionUpdate":"subagent_finished","child_session_id":"child-456"}}}"#.into())
        .unwrap();
    let _: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader))
            .await
            .unwrap()
            .unwrap();

    // Notification on finished child session should NOT be routed
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"child-456","update":{"sessionUpdate":"message_delta"}}}"#.into())
        .unwrap();
    let timeout_result: Result<Result<ServerMessage, _>, _> =
        tokio::time::timeout(Duration::from_millis(100), read_message(&mut reader)).await;
    assert!(
        timeout_result.is_err(),
        "Notification for finished child session should not be routed"
    );

    cancel.cancel();
}

#[tokio::test]
async fn subagent_child_session_not_leaked_to_other_client() {
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    // Client A owns parent session
    let (mut reader_a, mut writer_a) = connect_and_register(&sock_path, "test-a").await;
    write_message(
        &mut writer_a,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-parent","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // SubagentSpawned with ext/notification wrapper format
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"_echo.agent/session_notification","params":{"method":"echo.agent/session_notification","params":{"sessionId":"sess-parent","update":{"sessionUpdate":"subagent_spawned","child_session_id":"child-789"}}}}"#.into())
        .unwrap();
    let _: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_a))
            .await
            .unwrap()
            .unwrap();

    // Client B connects and becomes last_active_client
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "test-b").await;
    write_message(
        &mut writer_b,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"initialize","id":2,"params":{}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Child session notification should go to Client A, not B
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"echo.agent/session_notification","params":{"sessionId":"child-789","update":{"sessionUpdate":"message_delta"}}}"#.into())
        .unwrap();

    let msg: ServerMessage =
        tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_a))
            .await
            .expect("Client A should receive child session notification")
            .unwrap();
    assert!(matches!(msg, ServerMessage::Acp { .. }));

    let timeout_result: Result<Result<ServerMessage, _>, _> =
        tokio::time::timeout(Duration::from_millis(100), read_message(&mut reader_b)).await;
    assert!(
        timeout_result.is_err(),
        "Client B should NOT receive child session notification"
    );

    cancel.cancel();
}

#[tokio::test]
async fn leader_client_id_unicasts_to_target_only() {
    // A notification carrying `_meta["echo.agent/leaderClientId"]` (as the agent
    // stamps onto every session/load replay line) must be routed to ONLY that
    // client, even when another client is attached to the same session.
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    // Register two clients, capturing each one's assigned ClientId.
    async fn register_capture(
        sock_path: &std::path::Path,
        client_type: &str,
    ) -> (
        tokio::io::ReadHalf<LeaderStream>,
        tokio::io::WriteHalf<LeaderStream>,
        u64,
    ) {
        let stream = LeaderStream::connect(sock_path).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);
        write_message(
            &mut writer,
            &ClientMessage::Register {
                client_type: client_type.into(),
                mode: ClientMode::Stdio,
                capabilities: ClientCapabilities::default(),
            },
        )
        .await
        .unwrap();
        let msg: ServerMessage = read_message(&mut reader).await.unwrap();
        let client_id = match msg {
            ServerMessage::Registered { client_id, .. } => client_id,
            other => panic!("Expected Registered, got {:?}", other),
        };
        (reader, writer, client_id)
    }

    let (mut reader_a, _writer_a, id_a) = register_capture(&sock_path, "test-a").await;
    let (mut reader_b, _writer_b, _id_b) = register_capture(&sock_path, "test-b").await;

    // Agent emits a replay notification tagged for client A only.
    response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"agent_message_chunk"}},"_meta":{{"echo.agent/leaderClientId":{}}}}}}}"#,
            id_a
        ))
        .unwrap();

    // Client A receives it.
    let msg = tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_a))
        .await
        .expect("Client A should receive the tagged replay notification")
        .unwrap();
    assert!(matches!(msg, ServerMessage::Acp { .. }));

    // Client B must NOT receive it (no broadcast).
    let timeout_result: Result<Result<ServerMessage, _>, _> =
        tokio::time::timeout(Duration::from_millis(100), read_message(&mut reader_b)).await;
    assert!(
        timeout_result.is_err(),
        "Client B must not receive a notification tagged for client A"
    );

    cancel.cancel();
}

#[tokio::test]
async fn leader_client_id_dropped_when_target_disconnected() {
    // Regression: a replay line tagged for a client that disconnected
    // mid-replay must be DROPPED, not fall through to the subscriber
    // broadcast — other subscribers would render the full `isReplay`
    // transcript into an uncleared scrollback (duplicated history).
    let temp = TempDir::new().unwrap();
    let (sock_path, cancel, response_tx) = setup_persistent_server(&temp).await;

    // Client A (the loader) registers, then disconnects.
    let stream_a = LeaderStream::connect(&sock_path).await.unwrap();
    let (mut reader_a, mut writer_a) = tokio::io::split(stream_a);
    write_message(
        &mut writer_a,
        &ClientMessage::Register {
            client_type: "test-a".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        },
    )
    .await
    .unwrap();
    let id_a = match read_message(&mut reader_a).await.unwrap() {
        ServerMessage::Registered { client_id, .. } => client_id,
        other => panic!("Expected Registered, got {:?}", other),
    };

    // Client B subscribes to the session A is about to load, so a
    // fallthrough to the subscriber broadcast WOULD reach it.
    let (mut reader_b, mut writer_b) = connect_and_register(&sock_path, "test-b").await;
    write_message(
        &mut writer_b,
        &ClientMessage::Acp {
            payload: r#"{"jsonrpc":"2.0","method":"session/prompt","id":1,"params":{"sessionId":"sess-1","prompt":[]}}"#.into(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A disconnects (loader gone mid-replay).
    drop(reader_a);
    drop(writer_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Agent emits replay lines still tagged for A: the direct ACP shape
    // (`params._meta`) and the nested ext/notification shape
    // (`params.params._meta`) the EchoAgent `session/update` envelope uses.
    response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"agent_message_chunk"}},"_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}}}}}}"#,
            id_a
        ))
        .unwrap();
    response_tx
        .send(format!(
            r#"{{"jsonrpc":"2.0","method":"_echo.agent/session/update","params":{{"params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"hook_annotation","message":"m"}},"_meta":{{"isReplay":true,"echo.agent/leaderClientId":{}}}}}}}}}"#,
            id_a
        ))
        .unwrap();

    // Subscriber B must NOT receive either orphaned replay line.
    let timeout_result: Result<Result<ServerMessage, _>, _> =
        tokio::time::timeout(Duration::from_millis(150), read_message(&mut reader_b)).await;
    assert!(
        timeout_result.is_err(),
        "A targeted replay line for a disconnected loader must be dropped, not broadcast"
    );

    // The session routing still works for untagged live lines (B gets them).
    response_tx
        .send(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk"}}}"#.into())
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_millis(200), read_message(&mut reader_b))
        .await
        .expect("Subscriber B should still receive untagged live notifications")
        .unwrap();
    assert!(matches!(msg, ServerMessage::Acp { .. }));

    cancel.cancel();
}
