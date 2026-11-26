use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use super::transport::LeaderStream;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::protocol::{
    ClientCapabilities, ClientMessage, ClientMode, ControlCommand, ControlPayload,
    LeaderCapabilities, ProtocolError, ServerMessage, read_message, write_message,
};
use crate::cpu_profile::ControlError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_millis(100);
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
/// Interval for sending keepalive pings to detect dead connections
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Timeout for receiving registration response from server.
/// This prevents indefinite hangs if the server doesn't respond.
const REGISTRATION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for waiting for `LeaderReady` after a `Registered { ready: false }`.
///
/// The leader signals readiness right after its bounded sign-in
/// (`STARTUP_AUTH_TIMEOUT`); model/settings prefetch runs off the readiness path
/// and the leader never opens a browser OAuth flow. This therefore only needs to
/// cover that bounded auth plus margin, matching the client connect ceiling.
const LEADER_READY_TIMEOUT: Duration = crate::http::MIN_CLIENT_CONNECT_TIMEOUT;

/// Reason the client disconnected from the leader server.
///
/// Exposed via a `watch` channel so callers (e.g., reconnection logic)
/// can determine why the connection ended and decide whether to retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// Connection is still alive (initial state).
    Connected,
    /// Server sent an explicit `Shutdown` message (planned shutdown, e.g., auto-update).
    LeaderShutdown,
    /// Connection closed without a shutdown message (crash, kill, network error).
    ConnectionLost,
    /// Client initiated the disconnect (called `cancel()`).
    ClientInitiated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderRegistration {
    pub client_id: u64,
    pub leader_protocol_version: Option<u32>,
    pub leader_binary_version: Option<String>,
    pub leader_capabilities: Option<LeaderCapabilities>,
}

impl LeaderRegistration {
    /// Whether the connected leader advertises the `RelaunchForUpdate` control.
    pub fn supports_relaunch(&self) -> bool {
        self.leader_capabilities
            .as_ref()
            .is_some_and(|c| c.relaunch_v1)
    }
}

type ControlResponse = Result<ControlPayload, ControlError>;

/// Client-side handle for communicating with the leader server.
///
/// The client maintains an IPC connection to the leader and provides
/// send/receive channels for ACP messages. It automatically handles
/// registration and keepalive pings.
///
/// When the connection ends, the reason is published to a `watch` channel
/// accessible via [`disconnect_reason()`](Self::disconnect_reason). Callers
/// can use this to decide whether to attempt reconnection.
///
/// If the server sent a [`ServerMessage::ShuttingDown`] before closing, the
/// shutdown reason is available via [`shutting_down_reason()`](Self::shutting_down_reason).
pub struct LeaderClient {
    outbound_tx: mpsc::UnboundedSender<ClientMessage>,
    acp_rx: mpsc::UnboundedReceiver<String>,
    pending_control: Arc<Mutex<HashMap<String, oneshot::Sender<ControlResponse>>>>,
    next_request_id: Arc<AtomicU64>,
    registration: LeaderRegistration,
    cancel: CancellationToken,
    disconnect_rx: watch::Receiver<DisconnectReason>,
    /// Last `ShuttingDown` reason received from the server, or `None` if no
    /// `ShuttingDown` message has arrived yet (unplanned disconnect or still connected).
    shutting_down_rx: watch::Receiver<Option<super::protocol::ShutdownReason>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Connection failed after {0} attempts: {1}")]
    Connect(u32, std::io::Error),
    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("Registration failed: {0}")]
    Registration(String),
    #[error("Connection timeout after {0:?}")]
    Timeout(Duration),
    #[error("Connection closed: leader server shut down or disconnected")]
    ConnectionClosed,
    #[error("Control response channel closed before a response was received")]
    ControlResponseDropped,
    #[error("Control command unsupported by connected leader: {0}")]
    UnsupportedControl(String),
}

impl LeaderClient {
    pub async fn connect(
        socket_path: PathBuf,
        client_type: &str,
        mode: ClientMode,
        capabilities: ClientCapabilities,
    ) -> Result<Self, ClientError> {
        let stream = connect_with_retry(&socket_path).await?;
        let (reader, writer) = tokio::io::split(stream);

        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let (from_server_tx, from_server_rx) = mpsc::unbounded_channel();
        let pending_control = Arc::new(Mutex::new(HashMap::new()));
        let cancel = CancellationToken::new();
        let (disconnect_tx, disconnect_rx) = watch::channel(DisconnectReason::Connected);
        // Tracks the most recent ShuttingDown reason from the server.
        // None = no ShuttingDown seen; Some(reason) = last reason received.
        let (shutting_down_tx, shutting_down_rx) =
            watch::channel::<Option<super::protocol::ShutdownReason>>(None);
        // Register with server
        let (registration, _leader_ready_at_registration) = register(
            writer,
            reader,
            client_type,
            mode,
            capabilities,
            outbound_rx,
            from_server_tx,
            pending_control.clone(),
            cancel.clone(),
            disconnect_tx,
            shutting_down_tx,
        )
        .await?;
        debug!(
            client_id = registration.client_id,
            ?mode,
            "Connected to leader"
        );

        Ok(Self {
            outbound_tx,
            acp_rx: from_server_rx,
            pending_control,
            next_request_id: Arc::new(AtomicU64::new(1)),
            registration,
            cancel,
            disconnect_rx,
            shutting_down_rx,
        })
    }

    /// Returns a receiver for the most recent `ShuttingDown` reason sent by the server.
    ///
    /// - `None` — no `ShuttingDown` message has been received yet (still connected, or
    ///   the server closed the connection without a planned shutdown announcement).
    /// - `Some(reason)` — the server announced a planned shutdown with this reason.
    ///   Use this to distinguish e.g. `AutoUpdate` (safe to reconnect immediately) from
    ///   `Manual` (may indicate a deliberate stop).
    pub fn shutting_down_reason(&self) -> watch::Receiver<Option<super::protocol::ShutdownReason>> {
        self.shutting_down_rx.clone()
    }

    /// Send an ACP message payload to the leader server.
    ///
    /// Returns an error if the connection has been closed.
    pub fn send(&self, payload: String) -> Result<(), ClientError> {
        self.outbound_tx
            .send(ClientMessage::Acp { payload })
            .map_err(|_| ClientError::ConnectionClosed)
    }

    pub async fn send_control(
        &self,
        command: ControlCommand,
    ) -> Result<Result<ControlPayload, ControlError>, ClientError> {
        let protocol_version = self.registration.leader_protocol_version.ok_or_else(|| {
            ClientError::UnsupportedControl(
                "leader is legacy and does not advertise protocol metadata".into(),
            )
        })?;
        if protocol_version != super::protocol::LEADER_PROTOCOL_VERSION {
            return Err(ClientError::UnsupportedControl(format!(
                "leader uses unsupported protocol version {}",
                protocol_version
            )));
        }
        let capabilities = self
            .registration
            .leader_capabilities
            .as_ref()
            .ok_or_else(|| {
                ClientError::UnsupportedControl(
                    "leader did not advertise capabilities metadata".into(),
                )
            })?;
        if !capabilities.control_v1 {
            return Err(ClientError::UnsupportedControl(
                "leader does not advertise control_v1 support".into(),
            ));
        }

        let request_id = self
            .next_request_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        let (tx, rx) = oneshot::channel();
        self.pending_control
            .lock()
            .await
            .insert(request_id.clone(), tx);

        if self
            .outbound_tx
            .send(ClientMessage::Control {
                request_id: request_id.clone(),
                command,
            })
            .is_err()
        {
            self.pending_control.lock().await.remove(&request_id);
            return Err(ClientError::ConnectionClosed);
        }

        rx.await.map_err(|_| ClientError::ControlResponseDropped)
    }

    pub async fn recv(&mut self) -> Option<String> {
        self.acp_rx.recv().await
    }

    pub fn registration(&self) -> &LeaderRegistration {
        &self.registration
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Get a receiver for the disconnect reason.
    ///
    /// The initial value is [`DisconnectReason::Connected`]. When the connection
    /// ends, the value changes to the specific reason (shutdown, lost, or
    /// client-initiated). Callers can use `changed().await` to wait for
    /// disconnection, or `borrow()` to check the current state.
    pub fn disconnect_reason(&self) -> watch::Receiver<DisconnectReason> {
        self.disconnect_rx.clone()
    }

    pub fn into_channels(
        self,
    ) -> (
        mpsc::UnboundedSender<String>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let outbound = self.outbound_tx;
        let acp_rx = self.acp_rx;
        let (acp_tx, mut local_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(payload) = local_rx.recv().await {
                if outbound.send(ClientMessage::Acp { payload }).is_err() {
                    break;
                }
            }
        });
        (acp_tx, acp_rx)
    }

    /// Decompose into raw channels plus the disconnect reason receiver.
    ///
    /// Like [`into_channels()`](Self::into_channels) but also returns the
    /// disconnect watch so the caller can observe why the connection ended.
    pub(crate) fn into_channels_with_disconnect(
        self,
    ) -> (
        mpsc::UnboundedSender<String>,
        mpsc::UnboundedReceiver<String>,
        watch::Receiver<DisconnectReason>,
    ) {
        let outbound = self.outbound_tx;
        let acp_rx = self.acp_rx;
        let disconnect_rx = self.disconnect_rx;
        let (acp_tx, mut local_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(payload) = local_rx.recv().await {
                if outbound.send(ClientMessage::Acp { payload }).is_err() {
                    break;
                }
            }
        });
        (acp_tx, acp_rx, disconnect_rx)
    }
}

async fn connect_with_retry<P: AsRef<Path>>(socket_path: P) -> Result<LeaderStream, ClientError> {
    let mut attempts = 0;
    loop {
        match tokio::time::timeout(CONNECT_TIMEOUT, LeaderStream::connect(socket_path.as_ref()))
            .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(_)) if attempts < MAX_RECONNECT_ATTEMPTS => {
                attempts += 1;
                debug!(
                    attempt = attempts,
                    max = MAX_RECONNECT_ATTEMPTS,
                    "Connection failed, retrying"
                );
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
            Ok(Err(e)) => return Err(ClientError::Connect(attempts + 1, e)),
            Err(_) => {
                return Err(ClientError::Timeout(CONNECT_TIMEOUT));
            }
        }
    }
}

async fn register(
    mut writer: WriteHalf<LeaderStream>,
    mut reader: ReadHalf<LeaderStream>,
    client_type: &str,
    mode: ClientMode,
    capabilities: ClientCapabilities,
    mut outbound_rx: mpsc::UnboundedReceiver<ClientMessage>,
    from_server_tx: mpsc::UnboundedSender<String>,
    pending_control: Arc<Mutex<HashMap<String, oneshot::Sender<ControlResponse>>>>,
    cancel: CancellationToken,
    disconnect_tx: watch::Sender<DisconnectReason>,
    shutting_down_tx: watch::Sender<Option<super::protocol::ShutdownReason>>,
) -> Result<(LeaderRegistration, bool), ClientError> {
    // Send registration
    write_message(
        &mut writer,
        &ClientMessage::Register {
            client_type: client_type.into(),
            mode,
            capabilities,
        },
    )
    .await?;

    // Wait for confirmation with timeout to prevent indefinite hangs
    let response: ServerMessage = match tokio::time::timeout(
        REGISTRATION_RESPONSE_TIMEOUT,
        read_message(&mut reader),
    )
    .await
    {
        Ok(Ok(msg)) => msg,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            return Err(ClientError::Timeout(REGISTRATION_RESPONSE_TIMEOUT));
        }
    };
    let (registration, leader_ready_at_registration) = match response {
        ServerMessage::Registered {
            client_id,
            ready,
            leader_protocol_version,
            leader_binary_version,
            leader_capabilities,
        } => (
            LeaderRegistration {
                client_id,
                leader_protocol_version,
                leader_binary_version,
                leader_capabilities,
            },
            ready,
        ),
        ServerMessage::Error { message, .. } => {
            return Err(ClientError::Registration(message));
        }
        _ => return Err(ClientError::Registration("Unexpected response".into())),
    };

    let client_id = registration.client_id;

    // If the leader was still initialising at registration time, block here until
    // it signals `LeaderReady`. This ensures `connect_or_spawn` only returns once
    // the leader is truly ready to forward ACP traffic, so callers never need
    // retry logic for `leader_starting` errors on their initial `initialize` request.
    if !leader_ready_at_registration {
        debug!(
            client_id,
            "Waiting for LeaderReady (leader still initialising)"
        );
        let ready_msg = tokio::time::timeout(LEADER_READY_TIMEOUT, read_message(&mut reader)).await;
        match ready_msg {
            Ok(Ok(ServerMessage::LeaderReady)) => {
                debug!(client_id, "Received LeaderReady; proceeding");
            }
            Ok(Ok(ServerMessage::Shutdown)) | Ok(Err(ProtocolError::ConnectionClosed)) => {
                // Leader startup failed (auth error, crash, etc.).
                return Err(ClientError::ConnectionClosed);
            }
            Ok(Ok(ServerMessage::ShuttingDown { .. })) => {
                // ShuttingDown precedes Shutdown — read one more message to confirm.
                match read_message(&mut reader).await {
                    Ok(ServerMessage::Shutdown) | Err(ProtocolError::ConnectionClosed) => {
                        return Err(ClientError::ConnectionClosed);
                    }
                    _ => return Err(ClientError::ConnectionClosed),
                }
            }
            Ok(Ok(other)) => {
                warn!(?other, "Unexpected message while waiting for LeaderReady");
                return Err(ClientError::Registration(
                    "Unexpected message while waiting for leader readiness".into(),
                ));
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Err(ClientError::Timeout(LEADER_READY_TIMEOUT)),
        }
    }

    // Spawn read/write tasks
    let cancel_read = cancel.clone();
    let disconnect_tx_read = disconnect_tx.clone();
    let pending_control_read = pending_control.clone();
    tokio::spawn(async move {
        let reason = loop {
            tokio::select! {
                biased;
                _ = cancel_read.cancelled() => break DisconnectReason::ClientInitiated,
                msg_result = read_message::<_, ServerMessage>(&mut reader) => {
                    match msg_result {
                        Ok(ServerMessage::Acp { payload }) => {
                            if from_server_tx.send(payload).is_err() {
                                break DisconnectReason::ClientInitiated;
                            }
                        }
                        Ok(ServerMessage::ControlResult { request_id, result }) => {
                            if let Some(tx) = pending_control_read.lock().await.remove(&request_id) {
                                let _ = tx.send(result);
                            }
                        }
                        Ok(ServerMessage::Pong) => {
                            trace!("Received keepalive pong from server");
                        }
                        Ok(ServerMessage::ShuttingDown { reason, delay_ms }) => {
                            warn!(?reason, delay_ms, "Leader server shutting down (advance notice)");
                            // Cache the reason so callers can distinguish AutoUpdate from
                            // Manual shutdowns without inspecting the ACP message stream.
                            let _ = shutting_down_tx.send(Some(reason));
                            // Don't break yet — wait for the actual Shutdown message.
                            // Callers watching disconnect_rx will see LeaderShutdown
                            // when the Shutdown message arrives.
                        }
                        Ok(ServerMessage::Shutdown) => {
                            warn!("Leader server shutdown received");
                            break DisconnectReason::LeaderShutdown;
                        }
                        Ok(ServerMessage::Registered { client_id, .. }) => {
                            warn!(client_id, "Unexpected Registered message after initial registration");
                        }
                        Ok(ServerMessage::LeaderReady) => {
                            // Benign: can arrive if readiness transitions between
                            // the borrow-check and the wait_for in run_client_session.
                            // Safe to ignore here — ACP is already forwarding.
                            trace!("Received LeaderReady after connect (already ready)");
                        }
                        Ok(ServerMessage::Error { code, message }) => {
                            warn!(code, message, "Server error received");
                        }
                        Err(ProtocolError::ConnectionClosed) => {
                            debug!("Connection closed by server");
                            break DisconnectReason::ConnectionLost;
                        }
                        Err(e) => {
                            warn!(error = %e, "Read error on IPC connection");
                            break DisconnectReason::ConnectionLost;
                        }
                    }
                }
            }
        };
        debug!(?reason, "Client read loop ended");
        pending_control_read.lock().await.clear();
        let _ = disconnect_tx_read.send(reason);
    });

    // Write task with keepalive pings
    tokio::spawn(async move {
        let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
        // Don't send immediately
        keepalive.tick().await;

        let reason = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = write_message(&mut writer, &ClientMessage::Disconnect).await;
                    break DisconnectReason::ClientInitiated;
                }
                Some(msg) = outbound_rx.recv() => {
                    if write_message(&mut writer, &msg).await.is_err() {
                        break DisconnectReason::ConnectionLost;
                    }
                }
                _ = keepalive.tick() => {
                    trace!("Sending keepalive ping to server");
                    if write_message(&mut writer, &ClientMessage::Ping).await.is_err() {
                        debug!("Failed to send keepalive ping, connection may be dead");
                        break DisconnectReason::ConnectionLost;
                    }
                }
            }
        };
        // Only set if the read loop hasn't already set a more specific reason
        // (e.g., LeaderShutdown is more informative than ConnectionLost from a
        // write failure that happened because the socket was already closing).
        if *disconnect_tx.borrow() == DisconnectReason::Connected {
            let _ = disconnect_tx.send(reason);
        }
    });

    Ok((registration, leader_ready_at_registration))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::cpu_profile::{ControlError, ControlErrorCode, ProfilerEngine};
    use crate::leader::server::{
        LeaderServerControlState, LeaderServerMetadata, spawn_leader_server,
    };
    use crate::leader::test_support::{
        FakeLeaderBehavior, FakeVersions, fake_caps, spawn_fake_leader,
    };
    use tempfile::TempDir;

    // --- Misbehaving-leader wire shapes (fake leaders, paused clock) ---
    //
    // `start_paused` auto-advances the client-side timeouts under test; the
    // fakes stall on cancellation, never timers, so the paused clock cannot
    // wake them (see `leader::test_support`).

    /// A leader stuck at `Registered { ready: false }` parks the client for
    /// the full readiness deadline, then surfaces a hard timeout. Pins the
    /// 300s wait-then-error behavior; there is no eviction/respawn fallback
    /// for a leader that hangs during initialisation yet.
    #[tokio::test(start_paused = true)]
    async fn connect_times_out_after_ready_deadline_when_leader_never_ready() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("never-ready.sock");
        let fake =
            spawn_fake_leader(sock_path.clone(), FakeLeaderBehavior::ReadyFalseForever).await;

        let started = tokio::time::Instant::now();
        let result = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await;
        let Err(err) = result else {
            panic!("a never-ready leader must not yield a connection");
        };

        assert!(
            matches!(err, ClientError::Timeout(t) if t == LEADER_READY_TIMEOUT),
            "expected Timeout({LEADER_READY_TIMEOUT:?}), got {err:?}"
        );
        assert!(
            started.elapsed() >= LEADER_READY_TIMEOUT,
            "client must park for the full readiness deadline before erroring"
        );

        fake.cancel();
    }

    /// A leader that accepts but never sends `Registered` trips the
    /// registration-response timeout, not an indefinite hang.
    #[tokio::test(start_paused = true)]
    async fn connect_times_out_when_leader_silent_after_accept() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("silent.sock");
        let fake =
            spawn_fake_leader(sock_path.clone(), FakeLeaderBehavior::SilentAfterAccept).await;

        let result = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await;
        let Err(err) = result else {
            panic!("a silent leader must not yield a connection");
        };
        assert!(
            matches!(err, ClientError::Timeout(t) if t == REGISTRATION_RESPONSE_TIMEOUT),
            "expected Timeout({REGISTRATION_RESPONSE_TIMEOUT:?}), got {err:?}"
        );

        fake.cancel();
    }

    /// A partial length prefix (leader stalls mid-frame) resolves via the
    /// registration timeout — the framing layer must not hang forever on a
    /// short read.
    #[tokio::test(start_paused = true)]
    async fn connect_times_out_on_partial_frame_header() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("partial.sock");
        let fake = spawn_fake_leader(
            sock_path.clone(),
            FakeLeaderBehavior::PartialFrame { bytes: 2 },
        )
        .await;

        let result = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await;
        let Err(err) = result else {
            panic!("a half-written frame must not yield a connection");
        };
        assert!(
            matches!(err, ClientError::Timeout(_)),
            "expected a timeout on the stalled frame, got {err:?}"
        );

        fake.cancel();
    }

    /// A well-framed but non-JSON body surfaces a protocol error immediately —
    /// no timeout, no hang.
    #[tokio::test(start_paused = true)]
    async fn connect_fails_fast_on_garbage_frame() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("garbage.sock");
        let fake = spawn_fake_leader(sock_path.clone(), FakeLeaderBehavior::GarbageFrame).await;

        let result = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await;
        let Err(err) = result else {
            panic!("a garbage frame must not yield a connection");
        };
        assert!(
            matches!(err, ClientError::Protocol(ProtocolError::InvalidJson(_))),
            "expected InvalidJson, got {err:?}"
        );

        fake.cancel();
    }

    /// Registration succeeds against a future-protocol leader (the field is
    /// informational at registration time), but the control surface rejects it.
    #[tokio::test(start_paused = true)]
    async fn wrong_protocol_version_registers_but_rejects_control() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("wrong-proto.sock");
        let fake = spawn_fake_leader(
            sock_path.clone(),
            FakeLeaderBehavior::Normal {
                versions: FakeVersions {
                    protocol_version: Some(999),
                    binary_version: Some(echo_agent_version::VERSION.to_string()),
                },
                caps: fake_caps(true, false),
            },
        )
        .await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();
        assert_eq!(client.registration().leader_protocol_version, Some(999));

        let err = client
            .send_control(ControlCommand::GetLeaderInfo)
            .await
            .expect_err("control must be rejected for an unsupported protocol version");
        assert!(
            matches!(err, ClientError::UnsupportedControl(_)),
            "expected UnsupportedControl, got {err:?}"
        );

        client.cancel();
        fake.cancel();
    }

    #[tokio::test]
    async fn connect_to_server() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        assert!(client.registration().client_id > 0);
        assert_eq!(
            client.registration().leader_protocol_version,
            Some(super::super::protocol::LEADER_PROTOCOL_VERSION)
        );
        assert_eq!(
            client.registration().leader_binary_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(
            client
                .registration()
                .leader_capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.control_v1)
        );

        // Cleanup
        client.cancel();
        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn control_request_roundtrip() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

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

        client.cancel();
        handle.cancel.cancel();
    }

    #[derive(Debug)]
    struct BlockingProfilerEngine {
        release_rx: Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), ControlError>>>>,
        started_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        stop_calls: Arc<Mutex<Vec<PathBuf>>>,
        svg_path: PathBuf,
    }

    impl ProfilerEngine for BlockingProfilerEngine {
        fn stop(self: Box<Self>) -> Result<(), ControlError> {
            self.stop_calls.lock().unwrap().push(self.svg_path.clone());
            if let Some(started_tx) = self.started_tx.lock().unwrap().take() {
                let _ = started_tx.send(());
            }
            let result = self
                .release_rx
                .lock()
                .unwrap()
                .take()
                .expect("release receiver")
                .blocking_recv()
                .expect("release signal");
            if result.is_ok() {
                std::fs::write(&self.svg_path, "main;work 42\n").unwrap();
            }
            result
        }
    }

    #[tokio::test]
    async fn start_and_stop_cpu_profile_over_control_channel() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");
        let output_path = temp.path().join("runtime-profile.folded");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
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

        let start_result = client
            .send_control(ControlCommand::StartCpuProfile {
                output: Some(output_path.display().to_string()),
                frequency_hz: Some(200),
            })
            .await
            .unwrap();

        if runtime_cpu_profile {
            // In sandboxed CI (Bazel), pprof may report as supported at compile
            // time but fail at runtime because signal-based sampling is blocked.
            // Accept both success and InternalError (sandbox restriction).
            match start_result {
                Ok(started) => {
                    assert!(matches!(
                        started,
                        ControlPayload::CpuProfileStarted {
                            svg_path,
                            frequency_hz: 200,
                            ..
                        } if svg_path == output_path
                    ));

                    let status = client
                        .send_control(ControlCommand::CpuProfileStatus)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(matches!(
                        status,
                        ControlPayload::CpuProfileStatus {
                            active: true,
                            stopping: false,
                            svg_path: Some(path),
                            frequency_hz: Some(200),
                            ..
                        } if path == output_path
                    ));

                    let stopped = client
                        .send_control(ControlCommand::StopCpuProfile)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(matches!(
                        stopped,
                        ControlPayload::CpuProfileStopped { svg_path, .. } if svg_path == output_path
                    ));
                    assert!(output_path.exists());
                }
                Err(error) => {
                    // pprof compiled in but can't start (e.g. Bazel sandbox)
                    assert_eq!(
                        error.code,
                        crate::cpu_profile::ControlErrorCode::InternalError
                    );
                }
            }
        } else {
            let error = start_result.unwrap_err();
            assert_eq!(
                error.code,
                crate::cpu_profile::ControlErrorCode::RuntimeProfilingUnsupported
            );
        }

        client.cancel();
        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn in_flight_stop_surfaces_stopping_state_across_clients() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");
        let output_path = temp.path().join("runtime-profile-stopping.folded");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        if !handle
            .control_state
            .cpu_profile
            .lock()
            .runtime_cpu_profile()
        {
            handle.cancel.cancel();
            return;
        }

        let stop_calls = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let replacement_output = temp
            .path()
            .join("runtime-profile-stopping-replacement.folded");

        handle
            .control_state
            .cpu_profile
            .lock()
            .start_with_engine_for_test(
                crate::cpu_profile::CpuProfileStartOptions {
                    output: Some(output_path.clone()),
                    frequency_hz: Some(200),
                },
                Box::new(BlockingProfilerEngine {
                    release_rx: Mutex::new(Some(release_rx)),
                    started_tx: Mutex::new(Some(started_tx)),
                    stop_calls: stop_calls.clone(),
                    svg_path: output_path.clone(),
                }),
            )
            .unwrap();

        let client_b = LeaderClient::connect(
            sock_path.clone(),
            "test-b",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();
        let client_a = LeaderClient::connect(
            sock_path,
            "test-a",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        let stop_task =
            tokio::spawn(
                async move { client_a.send_control(ControlCommand::StopCpuProfile).await },
            );

        started_rx.await.unwrap();

        let status = client_b
            .send_control(ControlCommand::CpuProfileStatus)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            status,
            ControlPayload::CpuProfileStatus {
                active: false,
                stopping: true,
                svg_path: Some(path),
                frequency_hz: Some(200),
                ..
            } if path == output_path
        ));

        let leader_info = client_b
            .send_control(ControlCommand::GetLeaderInfo)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            leader_info,
            ControlPayload::LeaderInfo {
                cpu_profile_active: false,
                cpu_profile_stopping: true,
                profile_started_at: Some(_),
                ..
            }
        ));

        let start_err = client_b
            .send_control(ControlCommand::StartCpuProfile {
                output: Some(replacement_output.display().to_string()),
                frequency_hz: Some(200),
            })
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(start_err.code, ControlErrorCode::ProfileStopInProgress);

        let stop_err = client_b
            .send_control(ControlCommand::StopCpuProfile)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(stop_err.code, ControlErrorCode::ProfileStopInProgress);

        release_tx.send(Ok(())).unwrap();

        let stopped = stop_task.await.unwrap().unwrap().unwrap();
        assert!(matches!(
            stopped,
            ControlPayload::CpuProfileStopped { svg_path, .. } if svg_path == output_path
        ));
        assert_eq!(
            stop_calls.lock().unwrap().as_slice(),
            std::slice::from_ref(&output_path)
        );
        assert!(output_path.exists());

        let final_status = client_b
            .send_control(ControlCommand::CpuProfileStatus)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            final_status,
            ControlPayload::CpuProfileStatus {
                active: false,
                stopping: false,
                started_at: None,
                svg_path: None,
                frequency_hz: None,
            }
        ));

        client_b.cancel();
        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn send_and_receive_acp_message() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let mut handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        // Send message to server (with request ID for routing)
        let test_payload = r#"{"jsonrpc":"2.0","method":"test","id":1}"#;
        client.send(test_payload.into()).unwrap();

        // Receive it on server side - ID is now namespaced with client ID
        let payload = handle.acp_rx.recv().await.unwrap();
        // Verify it's valid JSON with a namespaced ID (format: "clientId|originalIdJson")
        let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(json["method"], "test");
        let id_str = json["id"].as_str().unwrap();
        assert!(
            id_str.contains('|'),
            "ID should be namespaced with pipe separator"
        );
        assert!(id_str.ends_with("|1"), "ID should end with original ID");

        // Send response back with the namespaced ID (server will restore original ID)
        let response = format!(r#"{{"jsonrpc":"2.0","result":{{}},"id":"{}"}}"#, id_str);
        handle.response_tx.send(response).unwrap();

        // Receive on client - ID should be restored to original
        let received = client.recv().await.unwrap();
        let received_json: serde_json::Value = serde_json::from_str(&received).unwrap();
        assert_eq!(received_json["id"], 1); // Original ID restored

        client.cancel();
        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn connect_with_yolo_mode() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let caps = ClientCapabilities {
            yolo_mode: true,
            default_model: None,
            ..Default::default()
        };
        let client = LeaderClient::connect(sock_path, "test", ClientMode::Stdio, caps)
            .await
            .unwrap();

        // Cleanup
        client.cancel();
        handle.cancel.cancel();
    }

    // --- DisconnectReason tests ---

    #[tokio::test]
    async fn disconnect_reason_starts_connected() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        // Should start as Connected
        let reason_rx = client.disconnect_reason();
        assert_eq!(*reason_rx.borrow(), DisconnectReason::Connected);

        client.cancel();
        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn disconnect_reason_client_initiated_on_cancel() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        let mut reason_rx = client.disconnect_reason();

        // Cancel the client
        client.cancel();

        // Wait for the disconnect reason to propagate
        let _ = tokio::time::timeout(Duration::from_secs(2), reason_rx.changed()).await;

        let reason = reason_rx.borrow().clone();
        assert_eq!(
            reason,
            DisconnectReason::ClientInitiated,
            "Expected ClientInitiated, got {:?}",
            reason
        );

        handle.cancel.cancel();
    }

    #[tokio::test]
    async fn disconnect_reason_on_server_shutdown() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        let handle = spawn_leader_server(sock_path.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        let mut reason_rx = client.disconnect_reason();

        // Kill the server — this triggers Shutdown broadcast then socket close
        handle.cancel.cancel();

        // Wait for the disconnect reason to propagate
        let _ = tokio::time::timeout(Duration::from_secs(2), reason_rx.changed()).await;

        let reason = reason_rx.borrow().clone();
        // Server cancellation sends Shutdown to connected clients, so we expect
        // either LeaderShutdown (if the Shutdown message arrives) or ConnectionLost
        // (if the socket closes before the message is read). Both are valid.
        assert!(
            reason == DisconnectReason::LeaderShutdown
                || reason == DisconnectReason::ConnectionLost,
            "Expected LeaderShutdown or ConnectionLost, got {:?}",
            reason
        );
    }

    #[tokio::test]
    async fn client_receives_shutting_down_then_disconnects_with_leader_shutdown() {
        let temp = TempDir::new().unwrap();
        let sock_path = temp.path().join("test.sock");

        // Use run_leader_server directly with no_exit_on_disconnect=true
        let (acp_tx, _acp_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_response_tx, response_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let client_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let control_state = LeaderServerControlState::new(LeaderServerMetadata {
            pid: std::process::id(),
            socket_path: sock_path.clone(),
            lock_path: sock_path.with_extension("lock"),
            ws_url_suffix: String::new(),
            leader_binary_version: env!("CARGO_PKG_VERSION").to_string(),
        });

        let cancel_clone = cancel.clone();
        let sock_clone = sock_path.clone();
        let cc = client_count.clone();
        tokio::spawn(async move {
            let _ = crate::leader::server::run_leader_server(
                sock_clone,
                acp_tx,
                response_rx,
                cancel_clone,
                true,
                cc,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), // agent_busy
                crate::agent::activity::AgentActivity::default(),
                tokio::sync::watch::channel(true).1,  // ready_rx
                tokio::sync::watch::channel(false).0, // relay_demand_tx
                tokio::sync::watch::channel(crate::leader::protocol::ShutdownReason::Manual).0, // shutdown_tx
                None, // use LEADER_VERSION constant
                control_state,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect via LeaderClient
        let client = LeaderClient::connect(
            sock_path,
            "test",
            ClientMode::Stdio,
            ClientCapabilities::default(),
        )
        .await
        .unwrap();

        let mut reason_rx = client.disconnect_reason();

        // Verify initial state is Connected
        assert_eq!(*reason_rx.borrow(), DisconnectReason::Connected);

        // Cancel the server — sends ShuttingDown then Shutdown immediately.
        // The client's read loop handles ShuttingDown (logs, doesn't break),
        // then receives Shutdown and sets DisconnectReason::LeaderShutdown.
        cancel.cancel();

        // Wait for disconnect reason to change
        let _ = tokio::time::timeout(Duration::from_secs(5), reason_rx.changed()).await;

        let final_reason = reason_rx.borrow().clone();
        // The final reason should be LeaderShutdown (from the Shutdown message)
        // or ConnectionLost (if the socket closed before Shutdown was read).
        // The key assertion: ShuttingDown alone did NOT cause the disconnect.
        assert!(
            final_reason == DisconnectReason::LeaderShutdown
                || final_reason == DisconnectReason::ConnectionLost,
            "Expected LeaderShutdown or ConnectionLost, got {:?}",
            final_reason
        );
    }
}
