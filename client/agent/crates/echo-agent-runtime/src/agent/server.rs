//! WebSocket server for remote agent connections.
//!
//! This module provides a WebSocket server that allows remote TUI clients to
//! connect to a echoagent agent running on a different machine.
//!
//! The agent persists across WebSocket reconnections: a single MvpAgent instance
//! is created on first connection and reused for all subsequent connections. This
//! ensures that session actors (and any in-flight prompts) survive client
//! disconnects — when a client reconnects and loads an existing session, ongoing
//! work continues to stream to the new connection.

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use axum::{
    Router,
    extract::{
        ConnectInfo, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, simplex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{info, warn};

use agent_client_protocol as acp;
use echo_agent_acp::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    AcpClientMessage, LineBufferedRead,
};

use crate::agent::config::{Config as AgentConfig, ModelEntry};
use crate::agent::models::{ModelFetchAuth, prefetch_models_blocking};
use crate::agent::mvp_agent::MvpAgent;

use indexmap::IndexMap;

/// Swappable destination for the relay task.
///
/// Points at the current ACP connection's gateway sender. When no client is
/// connected, the value is `None` and outbound messages are silently dropped
/// (matching the old behaviour where the gateway channel's receiver was simply
/// gone).
type RelayDest = Rc<RefCell<Option<mpsc::UnboundedSender<AcpClientMessage>>>>;

const MAX_BUFFER_SIZE: usize = 8 * 1024 * 1024;
const KEEPALIVE_INTERVAL_SECS: u64 = 15;

/// Configuration for the agent WebSocket server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind the server to
    pub bind_addr: SocketAddr,
    /// Secret token for client authentication (required)
    pub secret: String,
}

/// Shared state for the WebSocket server.
struct ServerState {
    agent_config: AgentConfig,
    secret: String,
    /// Persistent agent slot. Lazily initialised on first connection; protected
    /// by a tokio Mutex so the axum handler (which is `Send`) can acquire it.
    agent_slot: tokio::sync::Mutex<AgentSlot>,
    /// Monotonic id for each boot attempt. Reclaim/fail/drop must match it
    /// or a stale waiter can clobber a newer `Booting` and spawn a second agent.
    boot_gen: AtomicU64,
}

/// Lifecycle of the persistent agent OS thread.
enum AgentSlot {
    Down,
    /// In-flight spawn. `watch` wakes waiters when the slot leaves this state.
    Booting {
        boot_id: u64,
        rx: tokio::sync::watch::Receiver<()>,
    },
    Up(mpsc::UnboundedSender<NewConnectionChannels>),
}

fn is_boot_gen(slot: &AgentSlot, boot_id: u64) -> bool {
    matches!(slot, AgentSlot::Booting { boot_id: id, .. } if *id == boot_id)
}

/// Channels bridging a single WebSocket connection to the agent thread.
struct NewConnectionChannels {
    from_ws_rx: mpsc::UnboundedReceiver<String>,
    to_ws_tx: mpsc::UnboundedSender<String>,
}

/// Query parameters for WebSocket connection.
#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct WsQueryParams {
    #[serde(rename = "server-key")]
    pub server_key: Option<String>,
}

/// Validate the bearer token from request headers or query parameters.
fn validate_auth(headers: &HeaderMap, query: &WsQueryParams, expected_secret: &str) -> bool {
    // Try Authorization header
    if let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return token == expected_secret;
    }

    // Fall back to query parameter for browser connections
    if let Some(ref key) = query.server_key {
        return key == expected_secret;
    }

    false
}

/// WebSocket upgrade handler with authentication.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<WsQueryParams>,
) -> Response {
    // Validate secret token from header or query param
    if !validate_auth(&headers, &query, &state.secret) {
        warn!("Unauthorized connection attempt from {}", addr);
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
    }

    info!("Authenticated WebSocket connection from {}", addr);
    ws.on_upgrade(move |socket| handle_connection(socket, state, addr))
}

/// Start the persistent agent if needed and return its connection sender.
///
/// Ready covers runtime build only. Waiters do not hold the slot lock.
async fn ensure_persistent_agent(
    state: &ServerState,
) -> Option<mpsc::UnboundedSender<NewConnectionChannels>> {
    loop {
        let mut slot = state.agent_slot.lock().await;
        match &*slot {
            AgentSlot::Up(tx) if !tx.is_closed() => return Some(tx.clone()),
            AgentSlot::Up(_) => {
                warn!("Persistent agent thread died — will respawn");
                *slot = AgentSlot::Down;
            }
            AgentSlot::Booting { boot_id, rx } => {
                let boot_id = *boot_id;
                let rx = rx.clone();
                drop(slot);
                reclaim_abandoned_boot(&state.agent_slot, rx, boot_id).await;
            }
            AgentSlot::Down => {
                let (conn_tx, conn_rx) = mpsc::unbounded_channel();
                let (ready_tx, ready_rx) =
                    tokio::sync::oneshot::channel::<Result<(), std::io::ErrorKind>>();
                let (boot_tx, boot_rx) = tokio::sync::watch::channel(());
                let boot_id = state.boot_gen.fetch_add(1, Ordering::Relaxed) + 1;
                *slot = AgentSlot::Booting {
                    boot_id,
                    rx: boot_rx,
                };
                let agent_config = state.agent_config.clone();
                drop(slot);
                // Drop of this future (client gone mid-ready) must leave the
                // slot, or later callers spin forever on a dead watch.
                let mut boot = BootSlotGuard::new(&state.agent_slot, boot_tx, boot_id);
                if let Err(e) = thread::Builder::new()
                    .name("agent-persistent".into())
                    .spawn(move || persistent_agent_thread(agent_config, conn_rx, ready_tx))
                {
                    warn!(error = %e, "Failed to spawn persistent agent thread");
                    return fail_boot(&state.agent_slot, boot_id).await;
                }
                match ready_rx.await {
                    Ok(Ok(())) => {
                        let mut slot = state.agent_slot.lock().await;
                        if is_boot_gen(&slot, boot_id) {
                            *slot = AgentSlot::Up(conn_tx.clone());
                            drop(slot);
                            boot.notify_waiters();
                            info!("Persistent agent thread spawned");
                            return Some(conn_tx);
                        }
                        // Another attempt owns the slot; drop conn_tx so this
                        // thread's receiver closes instead of going live.
                        drop(slot);
                        boot.notify_waiters();
                    }
                    Ok(Err(kind)) => {
                        warn!(?kind, "Persistent agent runtime failed");
                        return fail_boot(&state.agent_slot, boot_id).await;
                    }
                    Err(_) => {
                        warn!("Persistent agent thread died during startup");
                        return fail_boot(&state.agent_slot, boot_id).await;
                    }
                }
            }
        }
    }
}

/// Resets a `Booting` slot whose watch sender vanished (cancel / panic).
///
/// `changed()` then returns immediately; without reclaim, waiters loop on
/// `Booting` forever and the agent can never start again.
async fn reclaim_abandoned_boot(
    slot: &tokio::sync::Mutex<AgentSlot>,
    mut rx: tokio::sync::watch::Receiver<()>,
    boot_id: u64,
) {
    if rx.changed().await.is_err() {
        let mut slot = slot.lock().await;
        if is_boot_gen(&slot, boot_id) {
            *slot = AgentSlot::Down;
        }
    }
}

/// Best-effort revert of `Booting` if `ensure_persistent_agent` is dropped
/// before it stores `Up` or `Down`. `try_lock` is enough: a waiter that
/// holds the mutex will see the dropped sender and reclaim.
#[must_use]
struct BootSlotGuard<'a> {
    slot: &'a tokio::sync::Mutex<AgentSlot>,
    boot_tx: Option<tokio::sync::watch::Sender<()>>,
    boot_id: u64,
}

impl<'a> BootSlotGuard<'a> {
    fn new(
        slot: &'a tokio::sync::Mutex<AgentSlot>,
        boot_tx: tokio::sync::watch::Sender<()>,
        boot_id: u64,
    ) -> Self {
        Self {
            slot,
            boot_tx: Some(boot_tx),
            boot_id,
        }
    }

    fn notify_waiters(&mut self) {
        if let Some(tx) = self.boot_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for BootSlotGuard<'_> {
    fn drop(&mut self) {
        let Some(tx) = self.boot_tx.take() else {
            return;
        };
        if let Ok(mut slot) = self.slot.try_lock()
            && is_boot_gen(&slot, self.boot_id)
        {
            *slot = AgentSlot::Down;
        }
        drop(tx);
    }
}

async fn fail_boot(
    slot: &tokio::sync::Mutex<AgentSlot>,
    boot_id: u64,
) -> Option<mpsc::UnboundedSender<NewConnectionChannels>> {
    let mut slot = slot.lock().await;
    if is_boot_gen(&slot, boot_id) {
        *slot = AgentSlot::Down;
    }
    None
}

fn persistent_agent_thread(
    agent_config: AgentConfig,
    conn_rx: mpsc::UnboundedReceiver<NewConnectionChannels>,
    ready_tx: tokio::sync::oneshot::Sender<Result<(), std::io::ErrorKind>>,
) -> std::io::Result<()> {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    let rt = match echo_agent_tty_utils::runtime::build_with_blocking_pool(builder.enable_all()) {
        Ok(rt) => {
            if ready_tx.send(Ok(())).is_err() {
                // Booter cancelled; drop `rt` so its keep-alive pool does
                // not overlap a respawn's 16-wide pre-warm (EAGAIN).
                return Ok(());
            }
            rt
        }
        Err(e) => {
            warn!(error = %e, "Failed to create runtime for agent");
            let _ = ready_tx.send(Err(e.kind()));
            return Err(e);
        }
    };

    // Same abandon after a successful ack: cancel drops `conn_tx`.
    if conn_rx.is_closed() {
        return Ok(());
    }

    // Prefetch is HTTP; it must not delay the first WS.
    let auth = agent_config.create_auth_manager().current();
    let fetch_auth = ModelFetchAuth::resolve(&agent_config.endpoints, auth.is_some());
    let prefetched_models = if auth.is_some()
        || agent_config.endpoints.has_custom_endpoint()
        || fetch_auth != ModelFetchAuth::Session
    {
        prefetch_models_blocking(&agent_config.endpoints, auth.as_ref(), fetch_auth)
    } else {
        None
    };
    info!("Prefetched models: {:?}", prefetched_models);

    if conn_rx.is_closed() {
        return Ok(());
    }

    let local_set = tokio::task::LocalSet::new();
    local_set.block_on(&rt, async move {
        run_persistent_agent(agent_config, conn_rx, prefetched_models).await
    });

    warn!("Persistent agent thread exiting");
    Ok(())
}

/// Handle an authenticated WebSocket connection.
///
/// On first connection, spawns a persistent agent thread that owns the MvpAgent.
/// On subsequent connections (reconnects), sends new WS channels to the existing
/// agent thread so that session actors can continue streaming to the new client.
async fn handle_connection(ws: WebSocket, state: Arc<ServerState>, peer_addr: SocketAddr) {
    info!("New WebSocket connection from {}", peer_addr);

    let (mut ws_write, mut ws_read) = ws.split();

    // Channels for bridging WS <-> Agent thread
    let (to_agent_tx, to_agent_rx) = mpsc::unbounded_channel::<String>();
    let (from_agent_tx, mut from_agent_rx) = mpsc::unbounded_channel::<String>();

    let attached = match ensure_persistent_agent(&state).await {
        Some(tx) => {
            let sent = tx
                .send(NewConnectionChannels {
                    from_ws_rx: to_agent_rx,
                    to_ws_tx: from_agent_tx,
                })
                .is_ok();
            if !sent {
                warn!("Failed to send connection channels to agent thread");
                let mut slot = state.agent_slot.lock().await;
                if let AgentSlot::Up(live) = &*slot
                    && live.is_closed()
                {
                    *slot = AgentSlot::Down;
                }
            }
            sent
        }
        None => {
            warn!("Persistent agent is not available");
            false
        }
    };
    if !attached {
        // Do not start the ping loop: the client would see a live socket
        // that never reaches the agent.
        let _ = ws_write
            .send(Message::Close(Some(CloseFrame {
                code: close_code::AGAIN,
                reason: "persistent agent unavailable".into(),
            })))
            .await;
        return;
    }

    // Task: Read from WS, send to agent thread
    let read_task = tokio::spawn(async move {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let text_str: &str = text.as_ref();
                    let trimmed = text_str.trim_end_matches(['\r', '\n']);
                    // Skip browser keepalive pings (non-JSON text)
                    if trimmed == "ping" || trimmed.is_empty() {
                        continue;
                    }
                    if to_agent_tx.send(trimmed.to_string()).is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(bin)) => {
                    if let Ok(s) = std::str::from_utf8(&bin) {
                        let trimmed = s.trim_end_matches(['\r', '\n']);
                        if trimmed == "ping" || trimmed.is_empty() {
                            continue;
                        }
                        if to_agent_tx.send(trimmed.to_string()).is_err() {
                            break;
                        }
                    }
                }
                Ok(Message::Close(frame)) => {
                    if let Some(f) = frame {
                        info!(
                            "WebSocket close from {}: {} {}",
                            peer_addr, f.code, f.reason
                        );
                    }
                    break;
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Err(e) => {
                    warn!("WebSocket read error from {}: {:?}", peer_addr, e);
                    break;
                }
            }
        }
    });

    // Task: Read from agent thread, send to WS (with keepalive)
    let write_task = tokio::spawn(async move {
        let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_INTERVAL_SECS));

        loop {
            tokio::select! {
                Some(msg) = from_agent_rx.recv() => {
                    if ws_write.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                _ = keepalive.tick() => {
                    if ws_write.send(Message::Ping(vec![].into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        _ = read_task => {}
        _ = write_task => {}
    }

    info!("WebSocket connection ended for {}", peer_addr);
}

/// Run the persistent agent on a dedicated thread with LocalSet.
///
/// The MvpAgent is created **once** and reused across WebSocket reconnections.
/// A persistent gateway channel ensures that session actors (which hold cloned
/// `GatewaySender` handles) can always send notifications. A relay task forwards
/// messages from the persistent channel to the *current* ACP connection's channel,
/// so notifications reach whichever client is currently connected.
async fn run_persistent_agent(
    agent_config: AgentConfig,
    mut connection_rx: mpsc::UnboundedReceiver<NewConnectionChannels>,
    prefetched_models: Option<IndexMap<String, ModelEntry>>,
) {
    // Persistent gateway channel — the MvpAgent and all session actors hold
    // clones of `gw_tx`. This channel survives across reconnections.
    let (gw_tx, mut gw_rx) = tokio::sync::mpsc::unbounded_channel::<AcpClientMessage>();
    let gateway = GatewaySender::new(gw_tx);

    // Create MvpAgent ONCE -- it persists for the lifetime of the server.
    let auth_manager = Arc::new(agent_config.create_auth_manager());
    // Proactive token refresh; runs until process exit.
    auth_manager.start_proactive_refresh(tokio_util::sync::CancellationToken::new());
    // Restore managed policy right before bootstrap reads it — the agent is created lazily here,
    // so an earlier restore could go stale before the gate.
    crate::managed_config::ensure_managed_policy_present(&auth_manager).await;
    crate::agent::app::apply_otel_config(&auth_manager, &agent_config.echo_agent_com_config);
    let agent = Rc::new(
        MvpAgent::new(gateway, &agent_config, auth_manager, prefetched_models)
            .unwrap_or_else(crate::agent::init::exit_on_config_error),
    );
    agent.models_manager.spawn_background_refresh();

    let relay_dest: RelayDest = Rc::new(RefCell::new(None));

    // Relay task: reads from the persistent gateway channel and forwards to
    // whichever ACP connection is currently active.
    let relay_dest_for_task = relay_dest.clone();
    tokio::task::spawn_local(async move {
        while let Some(msg) = gw_rx.recv().await {
            let maybe_tx = relay_dest_for_task.borrow().clone();
            if let Some(tx) = maybe_tx
                && tx.send(msg).is_err()
            {
                // Connection's gateway receiver was dropped — clear it.
                *relay_dest_for_task.borrow_mut() = None;
            }
            // If no connection, the message (and its response_tx) is dropped.
            // The caller (session actor) gets a send error which is already
            // handled with `let _ = ...`.
        }
    });

    // Accept new connections in a loop
    while let Some(channels) = connection_rx.recv().await {
        info!("Agent thread: setting up new ACP connection (reconnect)");
        setup_acp_connection(agent.clone(), channels, relay_dest.clone());
    }

    info!("Agent thread: connection channel closed, exiting");
}

/// Set up a new ACP connection for a WebSocket connection, reusing the existing
/// MvpAgent. The relay destination is updated so that session actor notifications
/// flow to the new client.
fn setup_acp_connection(
    agent: Rc<MvpAgent>,
    channels: NewConnectionChannels,
    relay_dest: RelayDest,
) {
    let NewConnectionChannels {
        mut from_ws_rx,
        to_ws_tx,
    } = channels;

    // Create new simplex IO streams for this ACP connection
    let (agent_read_rx, mut agent_read_tx) = simplex(MAX_BUFFER_SIZE);
    let (agent_write_rx, agent_write_tx) = simplex(MAX_BUFFER_SIZE);

    let incoming = agent_read_rx.compat();
    let outgoing = agent_write_tx.compat_write();

    // Create a per-connection gateway channel for the GatewayReceiver.
    // The relay task will forward persistent-channel messages here.
    let (conn_gw_tx, conn_gw_rx) = tokio::sync::mpsc::unbounded_channel::<AcpClientMessage>();

    // Point the relay at this new connection's channel
    *relay_dest.borrow_mut() = Some(conn_gw_tx);

    // Create new ACP connection reusing the same MvpAgent (via Rc clone).
    // `Agent` is implemented for `Rc<T: Agent>` so this works.
    let incoming = LineBufferedRead::spawn_local(incoming);
    let (conn, handle_io) = acp::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    tokio::task::spawn_local(
        GatewayReceiver::new(conn_gw_rx, conn)
            .with_on_meta(echo_agent_file_utils::trace_context::span_from_meta_traceparent)
            .run(),
    );

    // Task: Forward WS messages → agent (incoming ACP bytes)
    tokio::task::spawn_local(async move {
        while let Some(msg) = from_ws_rx.recv().await {
            // Log messages that lack both `id` and `method` — the ACP layer
            // only prints "received message with neither id nor method" without
            // the payload, making debugging impossible.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg)
                && v.get("id").is_none()
                && v.get("method").is_none()
            {
                warn!(
                    len = msg.len(),
                    "incoming WS message has neither id nor method"
                );
            }
            if agent_read_tx.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if agent_read_tx.write_all(b"\n").await.is_err() {
                break;
            }
        }
        // WS disconnected — the simplex writer is dropped, causing `handle_io`
        // to complete. The GatewayReceiver for this connection will also stop.
        // But the MvpAgent and session actors stay alive, ready for the next
        // connection.
    });

    // Task: Forward agent messages → WS (outgoing ACP bytes)
    tokio::task::spawn_local(async move {
        let mut reader = BufReader::new(agent_write_rx);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let msg = line.trim_end_matches(['\r', '\n']);
                    if !msg.is_empty() && to_ws_tx.send(msg.to_string()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Run the ACP IO handler — fire-and-forget since we don't block the
    // connection loop. It completes when the WS disconnects.
    tokio::task::spawn_local(async move {
        let _ = handle_io.await;
        info!("ACP connection IO handler completed");
    });
}

/// Run the agent WebSocket server.
///
/// This starts a WebSocket server that accepts authenticated connections from
/// remote TUI clients. A single agent instance is shared across all connections
/// (persisted across reconnections) so that in-flight session work survives
/// client disconnects.
///
/// # Arguments
/// * `config` - Server configuration (bind address and secret)
/// * `agent_config` - Agent configuration to use for each connection
///
/// # Example
/// ```ignore
/// let server_config = ServerConfig {
///     bind_addr: "0.0.0.0:9000".parse().unwrap(),
///     secret: "my-secret-token".to_string(),
/// };
/// run_agent_server(server_config, agent_config).await?;
/// ```
pub async fn run_agent_server(
    config: ServerConfig,
    agent_config: AgentConfig,
) -> anyhow::Result<()> {
    let state = Arc::new(ServerState {
        agent_config,
        secret: config.secret,
        agent_slot: tokio::sync::Mutex::new(AgentSlot::Down),
        boot_gen: AtomicU64::new(0),
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = TcpListener::bind(config.bind_addr).await?;
    info!("Agent server listening on ws://{}/ws", config.bind_addr);
    info!(
        "Clients should connect with: --remote ws://{}:{}/ws --secret <token>",
        config.bind_addr.ip(),
        config.bind_addr.port()
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
