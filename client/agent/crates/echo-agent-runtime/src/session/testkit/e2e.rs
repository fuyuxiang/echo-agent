//! In-process `session/load` harness: a real `MvpAgent` wired to a client over
//! ACP duplex pipes, so a test can time a real load round-trip without a
//! subprocess.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::{self as acp};
use echo_agent_acp::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::agent::config::Config as AgentConfig;
use crate::agent::mvp_agent::MvpAgent;

const DUPLEX_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// A completed `session/load` over the shared in-process harness. `client_conn`
/// is returned so the caller keeps the connection alive for any post-load
/// notifications (e.g. the re-advertise) it still wants to observe.
pub struct LoadedAgent {
    pub client_conn: acp::ClientSideConnection,
    pub load_started: Instant,
    pub load_elapsed: Duration,
}

/// Stand up a real `MvpAgent` over in-process ACP pipes wired to `client`, run
/// the initialize and authenticate handshake, then time one `session/load`
/// round-trip. Must run inside a `LocalSet`, since it spawns local tasks.
pub async fn load_session_via_agent<C: acp::Client + 'static>(
    client: C,
    client_type: &str,
    session_id: acp::SessionId,
    cwd: PathBuf,
) -> LoadedAgent {
    let agent_config = AgentConfig::default();
    let auth_manager = Arc::new(agent_config.create_auth_manager());
    let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(gw_tx);
    let agent = MvpAgent::new(gateway, &agent_config, auth_manager, None).expect("valid config");

    let (c2a_a, c2a_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (a2c_a, a2c_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);

    // Agent side.
    let agent_incoming = LineBufferedRead::spawn_local(c2a_b.compat());
    let (agent_conn, agent_io) =
        acp::AgentSideConnection::new(agent, a2c_a.compat_write(), agent_incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    tokio::task::spawn_local(
        GatewayReceiver::new(gw_rx, agent_conn)
            .with_on_meta(echo_agent_file_utils::trace_context::span_from_meta_traceparent)
            .run(),
    );
    tokio::task::spawn_local(agent_io);

    // Client side.
    let client_incoming = LineBufferedRead::spawn_local(a2c_b.compat());
    let (client_conn, client_io) =
        acp::ClientSideConnection::new(client, c2a_a.compat_write(), client_incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    tokio::task::spawn_local(client_io);

    use acp::Agent as _;

    let init = tokio::time::timeout(
        INIT_TIMEOUT,
        client_conn.initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(
                    acp::ClientCapabilities::new()
                        .fs(acp::FileSystemCapabilities::new())
                        .terminal(false),
                )
                .meta(
                    serde_json::json!({
                        "startupHints": { "nonInteractive": true, "skipGitStatus": true, "skipProjectLayout": true },
                        "clientType": client_type,
                        "clientVersion": "0.0-test",
                    })
                    .as_object()
                    .cloned(),
                ),
        ),
    )
    .await
    .expect("initialize timed out")
    .expect("initialize failed");

    // Best-effort auth: the mock backend accepts anything and `load` does not
    // require a prior success, so ignore any failure here.
    if let Some(method) = init
        .auth_methods
        .iter()
        .find(|m| &*m.id().0 == "echoagent.api_key")
    {
        let _ = client_conn
            .authenticate(
                acp::AuthenticateRequest::new(method.id().clone())
                    .meta(serde_json::json!({ "headless": true }).as_object().cloned()),
            )
            .await;
    }

    let load_started = Instant::now();
    tokio::time::timeout(
        LOAD_TIMEOUT,
        client_conn.load_session(acp::LoadSessionRequest::new(session_id, cwd)),
    )
    .await
    .expect("session/load timed out (>180s)")
    .expect("session/load failed");
    let load_elapsed = load_started.elapsed();

    LoadedAgent {
        client_conn,
        load_started,
        load_elapsed,
    }
}
