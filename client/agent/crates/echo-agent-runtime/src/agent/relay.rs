//! WebSocket relay connection management.
//!
//! This module provides a shared `RelayConnection` that handles the WebSocket
//! connection to the cloud.echo-agent.invalid relay server with automatic reconnection.
//! It is used by both `run_headless` and `run_leader` modes to avoid code duplication.
use super::proxy;
use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
use crate::{teprintln, tprintln};
use futures_util::{SinkExt as _, StreamExt as _};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{Message, Utf8Bytes, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
const KEEPALIVE_INTERVAL_SECS: u64 = 15;
/// Read-side liveness deadline. The write half pings every
/// `KEEPALIVE_INTERVAL_SECS`, and a healthy peer answers each ping with a
/// pong, so a live connection delivers an inbound frame at least that often.
/// If *nothing* arrives for this long the connection is treated as dead and
/// the session is torn down so the reconnect loop can take over.
///
/// Without this, a half-open TCP connection (e.g. the proxy/NAT leg still
/// ACKing our tiny pings while the upstream relay leg is gone) blocks
/// `ws_inbound.next()` forever and the agent never reconnects — sessions
/// stay bricked until the process is killed (server sees a 1006 close, the
/// client never notices).
const READ_LIVENESS_TIMEOUT_SECS: u64 = 4 * KEEPALIVE_INTERVAL_SECS;
/// Upper bound on a single auth-recovery attempt — a backstop against an
/// indefinitely wedged relay loop, NOT a bound on a healthy refresh. It must
/// stay comfortably above the refresh path's own internal worst case so it
/// only fires when something is truly stuck: `refresh_chain` waits up to 25s
/// for `auth.json.lock` (`REFRESH_LOCK_TIMEOUT`) before IdP IO — plus another
/// 25s if the suspend-only revalidate re-acquires — and the IdP IO has its
/// own timeouts (7s external refresher; 15s per OIDC request with short
/// retries). When this fires the recovery future is dropped (the file lock
/// releases on drop) and the loop falls through to reconnect backoff, which
/// retries recovery on the next 401.
const AUTH_RECOVERY_TIMEOUT_SECS: u64 = 180;
const BASE_DELAY_SECS: u64 = 1;
const MAX_DELAY_SECS: u64 = 60;
const CONNECT_TIMEOUT_SECS: u64 = 30;
/// JSON-RPC auth error code
const AUTH_ERROR_CODE: i64 = -32000;
use crate::auth::AuthManager;
/// Config for the cloud.echo-agent.invalid WebSocket relay. Fields are private so the only
/// constructor is [`RelayConfig::for_session`] — "no relay without a session
/// bearer" is a compile-time guarantee.
#[derive(Clone)]
pub struct RelayConfig {
    ws_url: String,
    ws_origin: String,
    token_header: String,
    auth: EchoAgentAuth,
    auth_manager: Option<Arc<AuthManager>>,
}
impl RelayConfig {
    /// Session gate: builds only for a cloud.echo-agent.invalid first-party session
    /// (`is_echo_agent_auth`: echo.agent-issuer OIDC or external credential) with a
    /// non-empty bearer. BYOK/ApiKey, non-echo.agent issuers (enterprise OIDC,
    /// third-party external providers), and deprecated WebLogin get `None`
    /// (relay-off; the leader still serves clients over IPC).
    pub(crate) fn for_session(
        session: &EchoAgentAuth,
        ctx: &EchoAgentComConfig,
        alpha_test_key: Option<String>,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Option<Self> {
        if !session.is_echo_agent_auth() || session.key.is_empty() {
            return None;
        }
        let _ = alpha_test_key;
        Some(Self {
            ws_url: ctx.echo_agent_ws_url.clone(),
            ws_origin: ctx.echo_agent_ws_origin.clone(),
            token_header: ctx.token_header.clone(),
            auth: session.clone(),
            auth_manager,
        })
    }
}
/// Callback type for first connection event.
pub(crate) type FirstConnectCallback = Box<dyn FnOnce() + Send + 'static>;
/// Handle to a running relay connection.
///
/// The relay maintains a persistent WebSocket connection to cloud.echo-agent.invalid with
/// automatic reconnection on disconnection.
pub struct RelayHandle {
    /// Cancel token to stop the relay connection loop
    cancel: CancellationToken,
}
impl RelayHandle {
    /// Stop the relay connection.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
    /// Check if the relay is still running.
    pub fn is_running(&self) -> bool {
        !self.cancel.is_cancelled()
    }
}
impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
/// Spawn a relay connection task that maintains a WebSocket connection.
///
/// The task runs in the background, automatically reconnecting on disconnection.
/// Messages from the relay are sent to `to_agent_tx`, and messages to send to
/// the relay should be sent via the returned sender.
///
/// # Arguments
/// * `config` - Relay connection configuration
/// * `to_agent_tx` - Channel to send messages received from the relay
/// * `parent_cancel` - Parent cancellation token (relay stops when parent is cancelled)
///
/// # Returns
/// A tuple of (sender for outbound messages, handle to control the relay)
pub fn spawn_relay_connection(
    config: RelayConfig,
    to_agent_tx: mpsc::UnboundedSender<String>,
    parent_cancel: CancellationToken,
) -> (mpsc::UnboundedSender<String>, RelayHandle) {
    spawn_relay_connection_with_callback(config, to_agent_tx, Some(parent_cancel), None)
}
/// Spawn a relay connection with an optional first-connection callback.
///
/// Same as `spawn_relay_connection` but allows providing a callback that will be
/// called once when the first successful connection is established.
pub(crate) fn spawn_relay_connection_with_callback(
    config: RelayConfig,
    to_agent_tx: mpsc::UnboundedSender<String>,
    parent_cancel: Option<CancellationToken>,
    on_first_connect: Option<FirstConnectCallback>,
) -> (mpsc::UnboundedSender<String>, RelayHandle) {
    let cancel = parent_cancel.map_or(CancellationToken::new(), |c| c.child_token());
    let cancel_clone = cancel.clone();
    let (agent_to_ws_tx, agent_to_ws_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        run_relay_loop(
            config,
            to_agent_tx,
            agent_to_ws_rx,
            cancel_clone,
            on_first_connect,
        )
        .await;
    });
    let handle = RelayHandle { cancel };
    (agent_to_ws_tx, handle)
}
/// Check if a connection error is an HTTP 401 from the WebSocket handshake.
fn is_handshake_unauthorized(err: &anyhow::Error) -> bool {
    use tokio_tungstenite::tungstenite::Error as WsError;
    err.downcast_ref::<WsError>()
        .map(|ws_err| {
            matches!(ws_err, WsError::Http(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED)
        })
        .unwrap_or(false)
}
/// Attempt auth recovery after a 401. Returns `true` to reconnect
/// immediately, `false` to exit or fall through to backoff.
async fn attempt_auth_recovery(
    config: &mut RelayConfig,
    cancel: &CancellationToken,
    context: &str,
) -> bool {
    let Some(ref am) = config.auth_manager else {
        teprintln!("Authentication required. Run `echoagent login` to re-authenticate.");
        cancel.cancel();
        return false;
    };
    info!("auth recovery: relay {context}, attempting refresh");
    let mut recovery = am.unauthorized_recovery(
        Some(config.auth.clone()),
        crate::auth::recovery::RecoverySource::Relay,
    );
    let recovered = match tokio::time::timeout(
        Duration::from_secs(AUTH_RECOVERY_TIMEOUT_SECS),
        recovery.next(),
    )
    .await
    {
        Ok(res) => res,
        Err(_) => {
            warn!(
                timeout_secs = AUTH_RECOVERY_TIMEOUT_SECS,
                "auth recovery: relay {context}, refresh timed out"
            );
            echo_agent_telemetry::unified_log::warn(
                "auth recovery: relay refresh timed out",
                None,
                Some(serde_json::json!({
                    "context": context,
                    "timeout_secs": AUTH_RECOVERY_TIMEOUT_SECS,
                })),
            );
            return false;
        }
    };
    match recovered {
        Ok(new_auth) if new_auth.key == config.auth.key => {
            info!("auth recovery: relay {context}, token unchanged, backing off");
            echo_agent_telemetry::unified_log::info(
                "auth recovery: relay token unchanged, backing off",
                None,
                Some(serde_json::json!({
                    "context": context,
                    "key_prefix": echo_agent_auth::bearer_suffix(&new_auth.key),
                })),
            );
            false
        }
        Ok(new_auth) => {
            info!("auth recovery: relay {context}, recovered, reconnecting");
            echo_agent_telemetry::unified_log::info(
                "auth recovery: relay recovered",
                None,
                Some(serde_json::json!({
                    "context": context,
                    "new_key_prefix": echo_agent_auth::bearer_suffix(&new_auth.key),
                })),
            );
            config.auth = new_auth;
            true
        }
        Err(e) if crate::auth::recovery::relay_should_cancel(&e) => {
            teprintln!("{e}");
            echo_agent_telemetry::unified_log::warn(
                "auth recovery: relay giving up (terminal)",
                None,
                Some(serde_json::json!({ "context": context, "error": format!("{e}") })),
            );
            cancel.cancel();
            false
        }
        Err(e) => {
            warn!(error = %e, "auth recovery: relay {context}, refresh failed");
            echo_agent_telemetry::unified_log::debug(
                "auth recovery: relay refresh failed",
                None,
                Some(serde_json::json!({ "context": context, "error": format!("{e}") })),
            );
            false
        }
    }
}
/// Internal function that runs the reconnection loop.
async fn run_relay_loop(
    mut config: RelayConfig,
    to_agent_tx: mpsc::UnboundedSender<String>,
    mut agent_to_ws_rx: mpsc::UnboundedReceiver<String>,
    cancel: CancellationToken,
    mut on_first_connect: Option<FirstConnectCallback>,
) {
    let mut reconnect_attempts = 0u32;
    let mut delay_secs = BASE_DELAY_SECS;
    let mut first_connection = true;
    let target_host = url::Url::parse(&config.ws_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()));
    let proxy_url = target_host
        .as_deref()
        .and_then(proxy::resolve_proxy_for_host);
    if let Some(ref url) = proxy_url {
        info!(
            proxy = %url,
            target = target_host.as_deref().unwrap_or("unknown"),
            "Using HTTP CONNECT proxy for relay connections"
        );
    }
    loop {
        if cancel.is_cancelled() {
            info!("Relay connection cancelled, stopping");
            break;
        }
        tracing::info!(
            target: crate::instrumentation::TARGET,
            event = "relay_connecting",
            ws_url = %config.ws_url,
            attempt = reconnect_attempts,
        );
        match connect_to_relay(&config, proxy_url.as_deref(), &cancel).await {
            Ok(ws) => {
                tracing::info!(
                    target: crate::instrumentation::TARGET,
                    event = "relay_connected",
                    ws_url = %config.ws_url,
                );
                reconnect_attempts = 0;
                delay_secs = BASE_DELAY_SECS;
                if first_connection {
                    if let Some(callback) = on_first_connect.take() {
                        callback();
                    }
                    first_connection = false;
                }
                let result =
                    run_websocket_session(ws, &to_agent_tx, &mut agent_to_ws_rx, &cancel).await;
                match result {
                    Ok(SessionEndReason::Normal) => {
                        info!("WebSocket session ended normally");
                    }
                    Ok(SessionEndReason::AuthError) => {
                        if attempt_auth_recovery(&mut config, &cancel, "Auth error").await {
                            continue;
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "WebSocket session ended with error");
                    }
                }
                if cancel.is_cancelled() {
                    break;
                }
                tracing::info!(
                    target: crate::instrumentation::TARGET,
                    event = "relay_disconnected",
                    ws_url = %config.ws_url,
                );
                tprintln!("Disconnected from EchoAgent WebSocket server");
                info!("WebSocket disconnected, will reconnect");
            }
            Err(e) => {
                let handshake_401 = is_handshake_unauthorized(&e);
                tracing::info!(
                    target: crate::instrumentation::TARGET,
                    event = "relay_connection_failed",
                    ws_url = %config.ws_url,
                    error = %e,
                    handshake_401,
                );
                if handshake_401 {
                    if attempt_auth_recovery(&mut config, &cancel, "Handshake 401").await {
                        continue;
                    }
                } else {
                    warn!(error = %e, "Failed to connect to WebSocket server");
                }
            }
        }
        if cancel.is_cancelled() {
            break;
        }
        reconnect_attempts += 1;
        delay_secs = std::cmp::min(delay_secs * 2, MAX_DELAY_SECS);
        info!(delay_secs, attempt = reconnect_attempts, "Reconnecting...");
        tprintln!(
            "Attempting to reconnect in {} seconds... (attempt #{})",
            delay_secs,
            reconnect_attempts
        );
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(delay_secs)) => {}
        }
    }
}
/// Reason why a WebSocket session ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionEndReason {
    /// Normal disconnection (server closed, network error, etc.)
    Normal,
    /// Authentication error that may be recoverable with token refresh
    AuthError,
}
/// Build an HTTP request with the relay authentication headers.
fn build_relay_request(config: &RelayConfig) -> anyhow::Result<axum::http::Request<()>> {
    let mut req = config.ws_url.clone().into_client_request()?;
    req.headers_mut().insert(
        "Origin",
        axum::http::header::HeaderValue::from_str(&config.ws_origin)?,
    );
    req.headers_mut().insert(
        "Authorization",
        axum::http::header::HeaderValue::from_str(&format!("Bearer {}", config.auth.key))?,
    );
    req.headers_mut().insert(
        "X-Echo-Agent-Token-Auth",
        axum::http::header::HeaderValue::from_str(&config.token_header)?,
    );
    req.headers_mut().insert(
        "x-userid",
        axum::http::header::HeaderValue::from_str(&config.auth.user_id)?,
    );
    req.headers_mut().insert(
        "x-echo-agent-client-version",
        axum::http::header::HeaderValue::from_static(echo_agent_version::VERSION),
    );
    req.headers_mut().insert(
        crate::http::CLIENT_MODE_HEADER,
        axum::http::header::HeaderValue::from_static(crate::http::process_client_mode()),
    );
    Ok(req)
}
/// Attempt to connect to the relay WebSocket server.
///
/// If `proxy_url` is `Some`, the connection is established through an HTTP
/// CONNECT tunnel.  Otherwise, a direct connection is used.
async fn connect_to_relay(
    config: &RelayConfig,
    proxy_url: Option<&str>,
    cancel: &CancellationToken,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let req = build_relay_request(config)?;
    let connect_timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);
    tokio::select! {
        _ = cancel.cancelled() => {
            anyhow::bail!("Connection cancelled");
        }
        result = tokio::time::timeout(connect_timeout, async {
            if let Some(proxy_url) = proxy_url {
                // Proxy path: open TCP to proxy, send CONNECT, then WS handshake.
                let target_host = req.uri().host()
                    .ok_or_else(|| anyhow::anyhow!("WebSocket URL has no host"))?;
                let target_port = req.uri().port_u16().unwrap_or(443);
                let tunneled_stream = proxy::connect_via_proxy(
                    proxy_url,
                    target_host,
                    target_port,
                ).await?;
                // Perform the WebSocket handshake over the tunneled stream.
                let (ws, resp) = tokio_tungstenite::client_async(req, tunneled_stream)
                    .await
                    .map_err(|e| anyhow::Error::from(e).context("WebSocket handshake via proxy failed"))?;
                Ok((ws, resp))
            } else {
                // The default connector never sees the shared trust config.
                let connector =
                    tokio_tungstenite::Connector::Rustls(echo_agent_extra_ca::rustls_client_config());
                connect_async_tls_with_config(req, None, false, Some(connector))
                    .await
                    .map_err(|e| anyhow::Error::from(e).context("WebSocket connection failed"))
            }
        }) => {
            match result {
                Ok(Ok((ws, resp))) => {
                    if let Some(proto) = resp.headers().get("Sec-WebSocket-Protocol") {
                        info!(subprotocol = ?proto, "WS subprotocol negotiated");
                    }
                    Ok(ws)
                }
                Ok(Err(e)) => Err(e),
                Err(_) => anyhow::bail!("WebSocket connection timed out after {} seconds", CONNECT_TIMEOUT_SECS),
            }
        }
    }
}
/// Run a single WebSocket session, handling messages until disconnection.
pub(crate) async fn run_websocket_session<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    to_agent_tx: &mpsc::UnboundedSender<String>,
    from_agent_rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
) -> anyhow::Result<SessionEndReason>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
{
    run_websocket_session_with_liveness(
        ws,
        to_agent_tx,
        from_agent_rx,
        cancel,
        Duration::from_secs(READ_LIVENESS_TIMEOUT_SECS),
    )
    .await
}
/// [`run_websocket_session`] with an explicit read-liveness window
/// (separate entry point so tests can use a short deadline).
pub(crate) async fn run_websocket_session_with_liveness<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    to_agent_tx: &mpsc::UnboundedSender<String>,
    from_agent_rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
    liveness: Duration,
) -> anyhow::Result<SessionEndReason>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
{
    let (mut ws_outbound, mut ws_inbound) = ws.split();
    let (auth_error_tx, mut auth_error_rx) = mpsc::channel::<()>(1);
    let cancel_read = cancel.clone();
    let read_from_ws = async move {
        loop {
            tokio::select! {
                _ = cancel_read.cancelled() => break,
                msg_res = tokio::time::timeout(liveness, ws_inbound.next()) => {
                    let Ok(msg_opt) = msg_res else {
                        // No frame (not even a pong for our keepalive pings)
                        // within the liveness window: the connection is dead
                        // or half-open. Break so the session ends and the
                        // reconnect loop takes over.
                        tprintln!("ws_inbound::liveness_timeout");
                        warn!(
                            timeout_secs = liveness.as_secs(),
                            "no WS traffic within liveness window, treating connection as dead"
                        );
                        echo_agent_telemetry::unified_log::warn(
                            "relay: read liveness timeout, reconnecting",
                            None,
                            Some(serde_json::json!({
                                "timeout_secs": liveness.as_secs(),
                            })),
                        );
                        break;
                    };
                    let Some(msg) = msg_opt else { break };
                    match msg {
                        Ok(Message::Text(text)) => {
                            let trimmed_end = text.trim_end_matches(['\r', '\n']);
                            if trimmed_end.is_empty() {
                                debug!("received empty/whitespace WS text frame - skipping");
                                continue;
                            }

                            let json: serde_json::Value = match serde_json::from_str(trimmed_end) {
                                Ok(v) => v,
                                Err(_) => {
                                    debug!("failed to parse WS message as JSON");
                                    continue;
                                }
                            };

                            if let Some(err) = json.get("error") {
                                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                                if code == AUTH_ERROR_CODE {
                                    // Signal auth error to the main loop
                                    let _ = auth_error_tx.send(()).await;
                                    return (false, true); // (normal_end, auth_error)
                                }
                                tracing::warn!(error_code = code, "Server error (skipping)");
                                continue;
                            }

                            match json.get("method").and_then(|m| m.as_str()) {
                                Some(method) => tprintln!("acp_inbound::{}", method),
                                None => tprintln!("ws_inbound::text"),
                            }
                            debug!(bytes = trimmed_end.len(), "received WS text -> agent");

                            if to_agent_tx.send(trimmed_end.to_string()).is_err() {
                                warn!("Failed to forward message to agent - channel closed");
                                break;
                            }
                        }
                        Ok(Message::Binary(bin)) => {
                            tprintln!("ws_inbound::binary");
                            if let Ok(s) = std::str::from_utf8(&bin) {
                                let s = s.trim_end_matches(['\r', '\n']);
                                if s.is_empty() {
                                    debug!("received empty WS binary frame - skipping");
                                    continue;
                                }
                                debug!(bytes = s.len(), "received WS binary(utf8) -> agent");
                                if to_agent_tx.send(s.to_string()).is_err() {
                                    break;
                                }
                            } else {
                                debug!("received non-utf8 WS binary frame - skipping");
                            }
                        }
                        Ok(Message::Close(frame_opt)) => {
                            tprintln!("ws_inbound::close");
                            if let Some(frame) = frame_opt {
                                info!(code = ?frame.code, reason = %frame.reason, "WS close received");
                            } else {
                                info!("WS close received (no frame)");
                            }
                            break;
                        }
                        Ok(Message::Ping(p)) => {
                            tprintln!("ws_inbound::ping");
                            debug!(len = p.len(), "received WS Ping");
                        }
                        Ok(Message::Pong(p)) => {
                            tprintln!("ws_inbound::pong");
                            debug!(len = p.len(), "received WS Pong");
                        }
                        Ok(Message::Frame(_)) => {
                            tprintln!("ws_inbound::frame");
                        }
                        Err(e) => {
                            tprintln!("ws_inbound::error::{:?}", &e);
                            warn!(error = ?e, "WS read error");
                            break;
                        }
                    }
                }
            }
        }
        (true, false)
    };
    let cancel_write = cancel.clone();
    let write_to_ws = async move {
        let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
        loop {
            tokio::select! {
                _ = cancel_write.cancelled() => break,
                msg_opt = from_agent_rx.recv() => {
                    match msg_opt {
                        Some(msg) => {
                            // Per-message logging is debug-only: at info level a
                            // streaming session mirrors every `session/update`
                            // delta here, and the full JSON parse + params
                            // re-format produced >100 MB of leader.log churn on
                            // dashboard-heavy machines. Skip the parse entirely
                            // unless debug logging is enabled.
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                if let Ok(json_val) =
                                    serde_json::from_str::<serde_json::Value>(&msg)
                                {
                                    let method = json_val.get("method").and_then(|m| m.as_str());
                                    let line_to_print = match method {
                                        Some("session/update") => {
                                            let params = json_val
                                                .get("params")
                                                .unwrap_or(&serde_json::Value::Null);
                                            format!("acp_outbound::session/update::{params}")
                                        }
                                        Some(m) => format!("acp_outbound::{m}"),
                                        None => "acp_outbound::response".to_string(),
                                    };
                                    debug!("{line_to_print}");
                                } else {
                                    debug!("acp_outbound::response");
                                }
                            }

                            if !msg.is_empty()
                                && let Err(e) = ws_outbound.send(Message::Text(Utf8Bytes::from(msg))).await
                            {
                                warn!(error = ?e, "failed to send to WS");
                                break;
                            }
                        }
                        None => {
                            info!("Agent outbound channel closed");
                            break;
                        }
                    }
                }
                _ = keepalive.tick() => {
                    tprintln!("ws::keep_alive_tick");
                    if let Err(e) = ws_outbound.send(Message::Ping(Vec::new().into())).await {
                        tprintln!("ws::keep_alive::error::{:?}", &e);
                        break;
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    tokio::select! {
        (_, auth_error) = read_from_ws => {
            info!("WebSocket read task completed (connection closed)");
            if auth_error {
                return Ok(SessionEndReason::AuthError);
            }
        }
        res = write_to_ws => {
            info!("WebSocket write task completed");
            res?;
        }
    }
    if auth_error_rx.try_recv().is_ok() {
        return Ok(SessionEndReason::AuthError);
    }
    Ok(SessionEndReason::Normal)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio_tungstenite::tungstenite::{Utf8Bytes, protocol::Role};
    /// Create an in-memory WebSocket pair (no network, no handshake needed).
    async fn ws_pair() -> (
        tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
        tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let client_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let server_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        (client_ws, server_ws)
    }
    #[test]
    fn test_handshake_401_detected_through_anyhow_context() {
        use tokio_tungstenite::tungstenite::Error as WsError;
        let resp = axum::http::Response::builder()
            .status(401)
            .body(None::<Vec<u8>>)
            .unwrap();
        let err = anyhow::Error::from(WsError::Http(resp)).context("WebSocket connection failed");
        assert!(is_handshake_unauthorized(&err));
    }
    #[test]
    fn test_handshake_non_401_and_non_ws_errors_rejected() {
        use tokio_tungstenite::tungstenite::Error as WsError;
        let resp = axum::http::Response::builder()
            .status(403)
            .body(None::<Vec<u8>>)
            .unwrap();
        let err = anyhow::Error::from(WsError::Http(resp)).context("WebSocket connection failed");
        assert!(!is_handshake_unauthorized(&err));
        let err = anyhow::anyhow!("some random error");
        assert!(!is_handshake_unauthorized(&err));
    }
    #[tokio::test]
    async fn test_ws_session_auth_error_returns_auth_error() {
        let (client_ws, server_ws) = ws_pair().await;
        let (mut server_tx, _server_rx) = server_ws.split();
        let (to_agent_tx, _to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let auth_error = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32000, "message": "Authentication required" }
            });
            let _ = server_tx
                .send(Message::Text(Utf8Bytes::from(auth_error.to_string())))
                .await;
            let _ = server_tx.close().await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session(client_ws, &to_agent_tx, &mut agent_out_rx, &cancel),
        )
        .await
        .expect("test timed out")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::AuthError);
    }
    #[tokio::test]
    async fn test_ws_session_non_auth_error_skipped() {
        let (client_ws, server_ws) = ws_pair().await;
        let (mut server_tx, _server_rx) = server_ws.split();
        let (to_agent_tx, _to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let other_error = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32600, "message": "Invalid Request" }
            });
            let _ = server_tx
                .send(Message::Text(Utf8Bytes::from(other_error.to_string())))
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = server_tx.close().await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session(client_ws, &to_agent_tx, &mut agent_out_rx, &cancel),
        )
        .await
        .expect("test timed out")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::Normal);
    }
    #[tokio::test]
    async fn test_ws_session_normal_close_returns_normal() {
        let (client_ws, server_ws) = ws_pair().await;
        let (mut server_tx, _server_rx) = server_ws.split();
        let (to_agent_tx, _to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _ = server_tx.send(Message::Close(None)).await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session(client_ws, &to_agent_tx, &mut agent_out_rx, &cancel),
        )
        .await
        .expect("test timed out")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::Normal);
    }
    #[tokio::test]
    async fn test_ws_session_read_liveness_timeout_ends_session() {
        let (client_ws, server_ws) = ws_pair().await;
        let _silent_server = server_ws;
        let (to_agent_tx, _to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session_with_liveness(
                client_ws,
                &to_agent_tx,
                &mut agent_out_rx,
                &cancel,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("session must end via read-liveness timeout instead of hanging")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::Normal);
    }
    #[tokio::test]
    async fn test_ws_session_inbound_traffic_resets_liveness_window() {
        let (client_ws, server_ws) = ws_pair().await;
        let (mut server_tx, _server_rx) = server_ws.split();
        let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            for i in 0..12 {
                let msg = json!({ "jsonrpc": "2.0", "method": "ping", "id": i });
                if server_tx
                    .send(Message::Text(Utf8Bytes::from(msg.to_string())))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = server_tx.close().await;
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session_with_liveness(
                client_ws,
                &to_agent_tx,
                &mut agent_out_rx,
                &cancel,
                Duration::from_millis(200),
            ),
        )
        .await
        .expect("test timed out")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::Normal);
        let mut forwarded = 0;
        while to_agent_rx.try_recv().is_ok() {
            forwarded += 1;
        }
        assert_eq!(forwarded, 12);
    }
    #[tokio::test]
    async fn test_ws_session_forwards_text_to_agent() {
        let (client_ws, server_ws) = ws_pair().await;
        let (mut server_tx, _server_rx) = server_ws.split();
        let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        let test_msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let msg_str = test_msg.to_string();
        tokio::spawn(async move {
            let _ = server_tx
                .send(Message::Text(Utf8Bytes::from(msg_str)))
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = server_tx.close().await;
        });
        let _result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session(client_ws, &to_agent_tx, &mut agent_out_rx, &cancel),
        )
        .await
        .expect("test timed out");
        let received = to_agent_rx
            .try_recv()
            .expect("should have forwarded message to agent");
        let received_json: serde_json::Value = serde_json::from_str(&received).unwrap();
        assert_eq!(received_json["method"], "initialize");
    }
    #[tokio::test]
    async fn test_ws_session_cancel_stops_session() {
        let (client_ws, server_ws) = ws_pair().await;
        let _server_ws = server_ws;
        let (to_agent_tx, _to_agent_rx) = mpsc::unbounded_channel::<String>();
        let (_agent_out_tx, mut agent_out_rx) = mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_websocket_session(client_ws, &to_agent_tx, &mut agent_out_rx, &cancel),
        )
        .await
        .expect("test timed out")
        .expect("session should not error");
        assert_eq!(result, SessionEndReason::Normal);
    }
    /// Helper to create a test EchoAgentAuth with the given key.
    fn test_auth(key: &str) -> EchoAgentAuth {
        EchoAgentAuth {
            key: key.to_string(),
            refresh_token: Some("rt".to_string()),
            ..EchoAgentAuth::test_default()
        }
    }
    #[test]
    fn for_session_builds_only_for_echo_agent_issuer() {
        use crate::auth::ECHO_AGENT_OAUTH2_ISSUER;
        let cfg = EchoAgentComConfig::default();
        let builds = |a: &EchoAgentAuth| RelayConfig::for_session(a, &cfg, None, None).is_some();
        let echoagent = EchoAgentAuth {
            auth_mode: AuthMode::Oidc,
            oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
            ..test_auth("echo-agent-bearer")
        };
        assert!(
            echoagent.is_echo_agent_auth(),
            "precondition: is_echo_agent_auth"
        );
        assert!(builds(&echoagent));
        let external_echo_agent = EchoAgentAuth {
            auth_mode: AuthMode::External,
            oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
            ..test_auth("ext-bearer")
        };
        assert!(
            external_echo_agent.is_echo_agent_auth(),
            "precondition: is_echo_agent_auth"
        );
        assert!(builds(&external_echo_agent));
        assert!(!builds(&EchoAgentAuth {
            key: String::new(),
            ..echoagent.clone()
        }));
        assert!(!builds(&EchoAgentAuth {
            auth_mode: AuthMode::ApiKey,
            ..test_auth("k")
        }));
        assert!(!builds(&EchoAgentAuth {
            auth_mode: AuthMode::External,
            ..test_auth("k")
        }));
        assert!(!builds(&EchoAgentAuth {
            auth_mode: AuthMode::WebLogin,
            ..test_auth("k")
        }));
        assert!(!builds(&EchoAgentAuth {
            auth_mode: AuthMode::Oidc,
            oidc_issuer: Some("https://login.acme-corp.example/oauth2".to_string()),
            ..test_auth("k")
        }));
        assert!(!builds(&EchoAgentAuth {
            auth_mode: AuthMode::External,
            oidc_issuer: Some("https://login.acme-corp.example/oauth2".to_string()),
            ..test_auth("k")
        }));
    }
    /// Helper: write a EchoAgentAuth to disk under the given scope.
    fn write_test_auth_to_disk(dir: &std::path::Path, scope: &str, auth: &EchoAgentAuth) {
        let path = dir.join("auth.json");
        let mut map = crate::auth::read_auth_json(&path).unwrap_or_default();
        map.insert(scope.to_owned(), auth.clone());
        let json = serde_json::to_string_pretty(&map).unwrap();
        std::fs::write(&path, json).unwrap();
    }
    /// Regression: `auth.json` vanishes (deleted/corrupt/externally
    /// removed) while the process still holds an expired access token +
    /// valid refresh token in `AuthManager` memory. Relay 401 recovery
    /// must drive the full refresh chain — mint a fresh token via the
    /// refresher and REWRITE `auth.json` — instead of dead-ending. A
    /// relay holding a private, refresher-less `AuthManager` fails this:
    /// it can only adopt sibling disk tokens, and there are none.
    #[tokio::test]
    async fn auth_recovery_refreshes_and_heals_missing_auth_json() {
        use crate::auth::ECHO_AGENT_OAUTH2_ISSUER;
        use crate::auth::refresh::{RefreshOutcome, TokenRefresher};
        use std::sync::atomic::AtomicU32;
        struct CountingRefresher {
            calls: Arc<AtomicU32>,
        }
        #[async_trait::async_trait]
        impl TokenRefresher for CountingRefresher {
            async fn refresh(
                &self,
                _reason: crate::auth::manager::RefreshReason,
            ) -> RefreshOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                RefreshOutcome::Success(Box::new(EchoAgentAuth {
                    key: "fresh-from-authority".into(),
                    auth_mode: AuthMode::Oidc,
                    oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
                    refresh_token: Some("rt-rotated".into()),
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                    ..EchoAgentAuth::test_default()
                }))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::auth::EchoAgentComConfig::default();
        let scope = cfg.auth_scope();
        let am = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url("http://127.0.0.1:1"),
        );
        let expired_session = EchoAgentAuth {
            auth_mode: AuthMode::Oidc,
            oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
            refresh_token: Some("rt-valid-unconsumed".into()),
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(14)),
            ..test_auth("expired-overnight")
        };
        am.hot_swap(expired_session.clone());
        assert!(
            !dir.path().join("auth.json").exists(),
            "precondition: no auth.json on disk"
        );
        let calls = Arc::new(AtomicU32::new(0));
        am.set_refresher(Arc::new(CountingRefresher {
            calls: calls.clone(),
        }));
        let mut config = RelayConfig::for_session(&expired_session, &cfg, None, Some(am.clone()))
            .expect("echo.agent OIDC session is relay-eligible");
        let cancel = CancellationToken::new();
        let recovered = attempt_auth_recovery(&mut config, &cancel, "test 401").await;
        assert!(recovered, "recovery must succeed via the shared refresher");
        assert!(!cancel.is_cancelled(), "relay must keep running");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one IdP refresh");
        assert_eq!(config.auth.key, "fresh-from-authority");
        let store = crate::auth::read_auth_json(&dir.path().join("auth.json"))
            .expect("auth.json must be recreated");
        let healed = store.get(&scope).expect("scope entry restored");
        assert_eq!(healed.key, "fresh-from-authority");
        assert_eq!(healed.refresh_token.as_deref(), Some("rt-rotated"));
    }
    /// Recovery returning the *unchanged* token (fresh-mint guard) must report
    /// no recovery — the caller then backs off before reconnecting instead of
    /// tight-looping — without cancelling the relay or touching the IdP.
    #[tokio::test]
    async fn attempt_auth_recovery_same_key_backs_off_without_cancel() {
        use crate::auth::ECHO_AGENT_OAUTH2_ISSUER;
        use crate::auth::refresh::{RefreshOutcome, TokenRefresher};
        struct PanicRefresher;
        #[async_trait::async_trait]
        impl TokenRefresher for PanicRefresher {
            async fn refresh(
                &self,
                _reason: crate::auth::manager::RefreshReason,
            ) -> RefreshOutcome {
                panic!("fresh-mint guard must keep recovery away from the IdP");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::auth::EchoAgentComConfig::default();
        let am = Arc::new(AuthManager::new(dir.path(), cfg.clone()));
        let fresh_session = EchoAgentAuth {
            auth_mode: AuthMode::Oidc,
            oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
            refresh_token: Some("rt-valid".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..test_auth("fresh-key")
        };
        am.hot_swap(fresh_session.clone());
        am.set_refresher(Arc::new(PanicRefresher));
        let mut config = RelayConfig::for_session(&fresh_session, &cfg, None, Some(am.clone()))
            .expect("echo.agent OIDC session is relay-eligible");
        let cancel = CancellationToken::new();
        let recovered = attempt_auth_recovery(&mut config, &cancel, "test 401").await;
        assert!(!recovered, "same-key recovery must take the backoff path");
        assert!(!cancel.is_cancelled(), "relay must keep reconnecting");
        assert_eq!(config.auth.key, "fresh-key", "config auth stays unchanged");
    }
    #[tokio::test]
    async fn test_auth_refresh_via_auth_manager_on_auth_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicU32::new(0));
        let count_clone = connection_count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let count = count_clone.clone();
                tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let (mut tx, _rx) = ws.split();
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        let auth_err = json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "error": { "code": -32000, "message": "Token expired" }
                        });
                        let _ = tx
                            .send(Message::Text(Utf8Bytes::from(auth_err.to_string())))
                            .await;
                    } else {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                });
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::auth::EchoAgentComConfig::default();
        let scope = cfg.auth_scope();
        let am = Arc::new(AuthManager::new(dir.path(), cfg));
        am.hot_swap(test_auth("old-key"));
        write_test_auth_to_disk(dir.path(), &scope, &test_auth("new-key"));
        let config = RelayConfig {
            ws_url: format!("ws://{}", addr),
            ws_origin: format!("http://{}", addr),
            token_header: "test-token".to_string(),
            auth: test_auth("old-key"),
            auth_manager: Some(am),
        };
        let cancel = CancellationToken::new();
        let (from_relay_tx, _from_relay_rx) = mpsc::unbounded_channel();
        let (_to_relay_tx, _handle) = spawn_relay_connection(config, from_relay_tx, cancel.clone());
        tokio::time::sleep(Duration::from_secs(3)).await;
        cancel.cancel();
        assert!(
            connection_count.load(Ordering::SeqCst) >= 2,
            "should have connected at least twice (original + after refresh), got {}",
            connection_count.load(Ordering::SeqCst)
        );
    }
    #[tokio::test]
    async fn test_auth_refresh_failure_continues_with_backoff() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicU32::new(0));
        let count_clone = connection_count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let count = count_clone.clone();
                tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    let (mut tx, _rx) = ws.split();
                    count.fetch_add(1, Ordering::SeqCst);
                    let auth_err = json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "error": { "code": -32000, "message": "Token expired" }
                    });
                    let _ = tx
                        .send(Message::Text(Utf8Bytes::from(auth_err.to_string())))
                        .await;
                });
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::auth::EchoAgentComConfig::default();
        let scope = cfg.auth_scope();
        let am = Arc::new(AuthManager::new(dir.path(), cfg));
        am.hot_swap(test_auth("old-key"));
        write_test_auth_to_disk(dir.path(), &scope, &test_auth("old-key"));
        let config = RelayConfig {
            ws_url: format!("ws://{}", addr),
            ws_origin: format!("http://{}", addr),
            token_header: "test-token".to_string(),
            auth: test_auth("old-key"),
            auth_manager: Some(am),
        };
        let cancel = CancellationToken::new();
        let (from_relay_tx, _from_relay_rx) = mpsc::unbounded_channel();
        let (_to_relay_tx, _handle) = spawn_relay_connection(config, from_relay_tx, cancel.clone());
        tokio::time::sleep(Duration::from_secs(4)).await;
        cancel.cancel();
        assert!(
            connection_count.load(Ordering::SeqCst) >= 2,
            "should have retried after failed refresh, got {}",
            connection_count.load(Ordering::SeqCst)
        );
    }
}
