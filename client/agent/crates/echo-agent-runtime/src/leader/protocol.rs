use std::io;
use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::cpu_profile::{ControlError, ProfileArtifactFormat};

const MAX_MESSAGE_SIZE: u32 = 64 * 1024 * 1024; // 64MB

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Message too large: {0} bytes (max: {MAX_MESSAGE_SIZE})")]
    MessageTooLarge(u32),
    #[error("Invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Connection closed")]
    ConnectionClosed,
}

pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, ProtocolError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::ConnectionClosed);
        }
        Err(e) => return Err(ProtocolError::Io(e)),
    }

    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge(len));
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> Result<(), ProtocolError> {
    let len = data.len() as u32;
    if len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge(len));
    }

    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R, T>(reader: &mut R) -> Result<T, ProtocolError>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let data = read_frame(reader).await?;
    Ok(serde_json::from_slice(&data)?)
}

pub async fn write_message<W, T>(writer: &mut W, msg: &T) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let data = serde_json::to_vec(msg)?;
    write_frame(writer, &data).await
}

/// Unique identifier for a connected client.
///
/// Each client gets a unique ID when connecting to the leader server.
/// IDs are monotonically increasing and wrap around at u64::MAX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub u64);

impl ClientId {
    /// Generate a new unique client ID.
    ///
    /// Uses an atomic counter that wraps around at u64::MAX.
    /// While collisions are theoretically possible after 2^64 IDs,
    /// this is practically impossible in real-world usage.
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        // Use wrapping_add to handle overflow gracefully
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(if id == 0 {
            COUNTER.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        })
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

/// Client mode determines how the leader handles communication for this client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMode {
    /// Headless mode (echoagent agent, echoagent agent headless) - uses websocket relay.
    /// Leader connects to websocket relay once and forwards messages.
    Headless,
    /// Stdio mode (echoagent agent stdio, echoagent -p) - uses local IPC.
    /// Client sends/receives ACP messages directly via IPC.
    Stdio,
}

/// Client capabilities reported during registration.
///
/// These capabilities are used by the leader to customize behavior for each client,
/// such as injecting settings into session requests.
pub const LEADER_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientCapabilities {
    /// Auto-approve all tool executions without confirmation (YOLO mode).
    /// When true, the leader will inject `yoloMode: true` into session/new requests.
    #[serde(default)]
    pub yolo_mode: bool,

    /// Classifier permission mode (auto). When true and not yolo, the leader
    /// injects `autoMode: true` into session/new and session/load `_meta`.
    #[serde(default)]
    pub auto_mode: bool,

    /// Default model ID to use for new sessions.
    /// When set, the leader will inject `modelId` into session/new requests
    /// (only if the request doesn't already specify a modelId).
    #[serde(default)]
    pub default_model: Option<String>,

    /// Client binary version (e.g., "0.1.150").
    /// Used by the leader to detect version mismatches after client auto-updates.
    /// If the client version differs from the leader's version, a warning is logged.
    #[serde(default)]
    pub client_version: Option<String>,

    /// Whether this client has advertised `echo.agent/codeNavigation.enabled`.
    /// When true, the leader injects `codeNavEnabled: true` into `session/new`
    /// and `session/load` requests so the agent can gate code-nav startup on a
    /// per-client basis rather than reading from shared last-initialized state.
    #[serde(default)]
    pub code_nav_enabled: bool,

    /// Whether the client handles terminal ACP messages (create, output, kill, etc.).
    /// When true, the leader injects `clientTerminal: true` into `session/new` and
    /// `session/load` so the agent routes terminal commands to the client via ACP
    /// instead of running them locally. Per-client so a TUI (`terminal: false`) and
    /// a web client (`terminal: true`) sharing the same leader get independent routing.
    #[serde(default)]
    pub terminal: bool,

    /// Whether the client handles filesystem ACP read/write messages.
    /// Same per-client isolation rationale as `terminal`.
    #[serde(default)]
    pub fs_read: bool,
    #[serde(default)]
    pub fs_write: bool,

    /// Whether this client will draw a status row (`echo.agent/statusLine`). When
    /// true, the leader injects `clientStatusLine: true` so the agent builds the
    /// payload for a client that asked rather than for whichever one started the
    /// process. The flag it sets is per session, so other subscribers of a
    /// shared session receive the payload too.
    #[serde(default)]
    pub status_line: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LeaderCapabilities {
    #[serde(default)]
    pub control_v1: bool,
    #[serde(default)]
    pub runtime_cpu_profile: bool,
    #[serde(default)]
    pub profile_formats: Vec<ProfileArtifactFormat>,
    #[serde(default)]
    pub workspace_exposure: bool,
    /// Whether the leader supports [`ControlCommand::RelaunchForUpdate`] — a
    /// disruptive, bounded-grace relaunch onto a freshly-installed binary
    /// (driven by `echoagent update`). Old leaders default to `false`, so a new
    /// client falls back to advising a manual restart (graceful degradation).
    #[serde(default)]
    pub relaunch_v1: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    GetLeaderInfo,
    CpuProfileStatus,
    StartCpuProfile {
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        frequency_hz: Option<i32>,
    },
    StopCpuProfile,
    WorkspaceStart {
        #[serde(default)]
        hub_url: Option<String>,
        cwd: String,
    },
    WorkspacePause,
    WorkspaceResume,
    WorkspaceStop,
    WorkspaceStatus,
    /// Ask the leader to relaunch onto a freshly-installed binary (driven by
    /// `echoagent update`). The leader stops admitting new turns, waits a bounded
    /// grace period for in-flight turns to finish, flushes session state, then
    /// exits with [`ShutdownReason::AutoUpdate`] so connected clients reconnect
    /// onto the new binary and restore their sessions via `session/load`.
    ///
    /// `to_version` is the version `echoagent update` just installed; the leader uses
    /// it to decline if it is already running that version or newer.
    RelaunchForUpdate {
        to_version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlPayload {
    LeaderInfo {
        pid: u32,
        socket_path: PathBuf,
        lock_path: PathBuf,
        ws_url_suffix: String,
        leader_protocol_version: u32,
        leader_binary_version: String,
        profiling_supported: bool,
        profiling_compiled_in: bool,
        cpu_profile_active: bool,
        #[serde(default)]
        cpu_profile_stopping: bool,
        profile_started_at: Option<String>,
        profile_formats: Vec<ProfileArtifactFormat>,
    },
    CpuProfileStatus {
        active: bool,
        #[serde(default)]
        stopping: bool,
        started_at: Option<String>,
        svg_path: Option<PathBuf>,
        frequency_hz: Option<i32>,
    },
    CpuProfileStarted {
        pid: u32,
        svg_path: PathBuf,
        frequency_hz: i32,
        started_at: String,
    },
    CpuProfileStopped {
        pid: u32,
        svg_path: PathBuf,
        started_at: String,
        stopped_at: String,
    },
    WorkspaceStatus {
        state: String,
        #[serde(default)]
        hub_url: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        uptime_ms: u64,
        active_tool_calls: u32,
        #[serde(default)]
        sessions: Vec<String>,
        pid: u32,
    },
    /// Ack for [`ControlCommand::RelaunchForUpdate`]: the leader accepted the
    /// request and will exit after a bounded grace period of `grace_ms`.
    Relaunching {
        from_version: String,
        to_version: String,
        grace_ms: u64,
    },
    /// Response to [`ControlCommand::RelaunchForUpdate`] when the leader will not
    /// relaunch — e.g. it is already running `to_version` or newer, or a relaunch
    /// is already in progress.
    RelaunchDeclined { reason: String },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        client_type: String,
        /// Client mode determines how leader handles this client's communication
        mode: ClientMode,
        #[serde(default)]
        capabilities: ClientCapabilities,
    },
    Acp {
        payload: String,
    },
    Control {
        request_id: String,
        command: ControlCommand,
    },
    Ping,
    Disconnect,
}

/// Reason for a planned leader shutdown, sent with [`ServerMessage::ShuttingDown`].
///
/// ## Runtime status
///
/// | Variant | Emitted today? | Notes |
/// |---------|---------------|-------|
/// | `AutoUpdate` | **Yes** — when `run_auto_update_checker` triggers shutdown | |
/// | `Manual` | **Yes** — default for SIGTERM, test cancellation, all other paths | |
/// | `IdleTimeout` | **No** — reserved for a future idle-timeout feature | |
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    /// Leader is shutting down to install a downloaded binary auto-update.
    /// Clients should reconnect immediately via `connect_or_spawn`; the new binary
    /// will be picked up automatically.
    AutoUpdate,
    /// Reserved for a future idle-timeout feature (no active clients for a configurable
    /// duration). **Not emitted in the current implementation.**
    IdleTimeout,
    /// Unspecified or externally-triggered shutdown (SIGTERM, programmatic cancel, etc.).
    Manual,
}

/// Old leaders that predate `ready` are already initialised, so default to `true`.
fn default_ready() -> bool {
    true
}

/// New fields must use `#[serde(default)]` — the leader and client can run different binary versions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Registration confirmation.
    ///
    /// `ready` indicates whether the leader has already completed its startup
    /// (auth + model prefetch). When `ready = false` the client **must** wait for a
    /// subsequent [`LeaderReady`](Self::LeaderReady) message before sending any ACP
    /// traffic — the server will hold the connection open until the leader is ready.
    Registered {
        client_id: u64,
        /// Whether the leader is fully initialised and ready to forward ACP traffic.
        #[serde(default = "default_ready")]
        ready: bool,
        #[serde(default)]
        leader_protocol_version: Option<u32>,
        #[serde(default)]
        leader_binary_version: Option<String>,
        #[serde(default)]
        leader_capabilities: Option<LeaderCapabilities>,
    },
    Acp {
        payload: String,
    },
    ControlResult {
        request_id: String,
        result: Result<ControlPayload, ControlError>,
    },
    Pong,
    Error {
        code: i32,
        message: String,
    },
    /// Advance notice of a planned shutdown. Sent before [`Shutdown`](Self::Shutdown)
    /// to give clients time to prepare for reconnection.
    ///
    /// Clients should treat this as a signal that [`Shutdown`](Self::Shutdown) is
    /// imminent and pre-arm their reconnection handlers (e.g. show a banner).
    ShuttingDown {
        reason: ShutdownReason,
        /// Milliseconds until the actual [`Shutdown`](Self::Shutdown) message.
        ///
        /// **Currently always `0`** — the server sends `Shutdown` immediately after
        /// `ShuttingDown` with no intervening sleep. Clients must not rely on this
        /// field providing a real grace window in the current implementation; treat
        /// `ShuttingDown` as equivalent to an imminent `Shutdown` regardless of this
        /// value.
        delay_ms: u64,
    },
    Shutdown,
    /// Sent by the server after a `Registered { ready: false }` once the leader
    /// finishes initialising. The client should treat this as the signal that
    /// ACP traffic will now be forwarded correctly.
    LeaderReady,
}

/// Extension methods injected into the agent, named as the agent matches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumIter)]
pub(crate) enum InternalMethod {
    AuthCleared,
    EvictSessions,
    ReloadAllMcpServers,
    ReloadModels,
    ReloadModelsCache,
    ReloadProjectMcpServers,
    ReloadSkills,
    ReloadWorkflows,
}

impl InternalMethod {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::AuthCleared => "echo.agent/internal/auth_cleared",
            Self::EvictSessions => "echo.agent/internal/evict_sessions",
            Self::ReloadAllMcpServers => "echo.agent/internal/reload_all_mcp_servers",
            Self::ReloadModels => "echo.agent/internal/reload_models",
            Self::ReloadModelsCache => "echo.agent/internal/reload_models_cache",
            Self::ReloadProjectMcpServers => "echo.agent/internal/reload_project_mcp_servers",
            Self::ReloadSkills => "echo.agent/internal/reload_skills",
            Self::ReloadWorkflows => "echo.agent/internal/reload_workflows",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        use strum::IntoEnumIterator;

        Self::iter().find(|method| method.name() == name)
    }

    /// The decoder routes a custom method to `ext_method` / `ext_notification`
    /// only when it carries the `_` prefix, and rejects the bare name.
    fn wire_name(self) -> String {
        format!("_{}", self.name())
    }
}

/// Not newline-terminated: the `acp_tx` forwarding loop appends the terminator.
pub(crate) fn internal_notification(method: InternalMethod, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method.wire_name(),
        "params": params,
    })
    .to_string()
}

/// Newline-terminated for direct injection.
pub(crate) fn internal_request_line(
    id: &str,
    method: InternalMethod,
    params: serde_json::Value,
) -> String {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method.wire_name(),
        "params": params,
    });
    format!("{msg}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let data = b"hello world";

        write_frame(&mut client, data).await.unwrap();
        let received = read_frame(&mut server).await.unwrap();

        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn message_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let msg = ClientMessage::Register {
            client_type: "test".into(),
            mode: ClientMode::Stdio,
            capabilities: ClientCapabilities::default(),
        };

        write_message(&mut client, &msg).await.unwrap();
        let received: ClientMessage = read_message(&mut server).await.unwrap();

        match received {
            ClientMessage::Register {
                client_type, mode, ..
            } => {
                assert_eq!(client_type, "test");
                assert_eq!(mode, ClientMode::Stdio);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[tokio::test]
    async fn control_message_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let msg = ClientMessage::Control {
            request_id: "req-1".into(),
            command: ControlCommand::StartCpuProfile {
                output: Some("/tmp/profile.folded".into()),
                frequency_hz: Some(250),
            },
        };

        write_message(&mut client, &msg).await.unwrap();
        let received: ClientMessage = read_message(&mut server).await.unwrap();

        assert!(matches!(
            received,
            ClientMessage::Control {
                request_id,
                command: ControlCommand::StartCpuProfile {
                    output: Some(output),
                    frequency_hz: Some(250),
                },
            } if request_id == "req-1" && output == "/tmp/profile.folded"
        ));
    }

    #[tokio::test]
    async fn connection_closed_on_eof() {
        let (client, mut server) = duplex(1024);
        drop(client);

        match read_frame(&mut server).await {
            Err(ProtocolError::ConnectionClosed) => {}
            other => panic!("expected ConnectionClosed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn rejects_oversized_messages() {
        let (mut client, mut server) = duplex(1024);

        // Write a length header claiming a huge message
        client
            .write_all(&(MAX_MESSAGE_SIZE + 1).to_be_bytes())
            .await
            .unwrap();

        match read_frame(&mut server).await {
            Err(ProtocolError::MessageTooLarge(size)) => {
                assert_eq!(size, MAX_MESSAGE_SIZE + 1);
            }
            other => panic!("expected MessageTooLarge, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn multiple_frames_in_sequence() {
        let (mut client, mut server) = duplex(4096);

        for i in 0..10 {
            let data = format!("message {}", i);
            write_frame(&mut client, data.as_bytes()).await.unwrap();
        }
        drop(client);

        for i in 0..10 {
            let received = read_frame(&mut server).await.unwrap();
            assert_eq!(received, format!("message {}", i).as_bytes());
        }
    }

    #[test]
    fn registered_serde_compatibility_without_optional_metadata() {
        let json = r#"{"type":"registered","client_id":7}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();

        assert!(matches!(
            msg,
            ServerMessage::Registered {
                client_id: 7,
                // `ready` defaults to `true` via `default_ready()` — old leaders
                // that predate the field are already initialised.
                ready: true,
                leader_protocol_version: None,
                leader_binary_version: None,
                leader_capabilities: None,
            }
        ));
    }

    #[test]
    fn registered_serde_compatibility_with_all_optional_metadata() {
        let msg = ServerMessage::Registered {
            client_id: 7,
            ready: true,
            leader_protocol_version: Some(LEADER_PROTOCOL_VERSION),
            leader_binary_version: Some("1.2.3".into()),
            leader_capabilities: Some(LeaderCapabilities {
                control_v1: true,
                runtime_cpu_profile: true,
                profile_formats: vec![ProfileArtifactFormat::Svg],
                workspace_exposure: true,
                relaunch_v1: true,
            }),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::Registered {
                client_id: 7,
                ready: true,
                leader_protocol_version: Some(LEADER_PROTOCOL_VERSION),
                leader_binary_version: Some(_),
                leader_capabilities: Some(LeaderCapabilities {
                    control_v1: true,
                    runtime_cpu_profile: true,
                    profile_formats,
                    workspace_exposure: true,
                    relaunch_v1: true,
                }),
            } if profile_formats == vec![ProfileArtifactFormat::Svg]
        ));
    }

    #[test]
    fn profile_artifact_format_serde_names_are_stable() {
        // Wire compat contract: `svg` must stay decodable (old leaders
        // advertise it), and `folded` is the name new binaries will start
        // advertising once the fleet can decode it. Renaming either variant
        // breaks the Registered handshake across version skew.
        assert_eq!(
            serde_json::to_string(&ProfileArtifactFormat::Svg).unwrap(),
            "\"svg\""
        );
        assert_eq!(
            serde_json::to_string(&ProfileArtifactFormat::Folded).unwrap(),
            "\"folded\""
        );
        let decoded: ProfileArtifactFormat = serde_json::from_str("\"svg\"").unwrap();
        assert_eq!(decoded, ProfileArtifactFormat::Svg);
    }

    #[test]
    fn control_payload_serde_defaults_new_stopping_flags() {
        let leader_info_json = r#"{
            "type":"leader_info",
            "pid":123,
            "socket_path":"/tmp/leader.sock",
            "lock_path":"/tmp/leader.lock",
            "ws_url_suffix":"suffix",
            "leader_protocol_version":1,
            "leader_binary_version":"1.2.3",
            "profiling_supported":true,
            "profiling_compiled_in":true,
            "cpu_profile_active":false,
            "profile_started_at":null,
            "profile_formats":["svg"]
        }"#;
        let status_json = r#"{
            "type":"cpu_profile_status",
            "active":false,
            "started_at":null,
            "svg_path":null,
            "frequency_hz":null
        }"#;

        let leader_info: ControlPayload = serde_json::from_str(leader_info_json).unwrap();
        let status: ControlPayload = serde_json::from_str(status_json).unwrap();

        assert!(matches!(
            leader_info,
            ControlPayload::LeaderInfo {
                cpu_profile_active: false,
                cpu_profile_stopping: false,
                profile_started_at: None,
                ..
            }
        ));
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
    }

    #[tokio::test]
    async fn workspace_control_command_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let msg = ClientMessage::Control {
            request_id: "ws-1".into(),
            command: ControlCommand::WorkspaceStart {
                hub_url: Some("wss://hub.example/v1/tools".into()),
                cwd: "/home/u/proj".into(),
            },
        };

        write_message(&mut client, &msg).await.unwrap();
        let received: ClientMessage = read_message(&mut server).await.unwrap();

        assert!(matches!(
            received,
            ClientMessage::Control {
                request_id,
                command: ControlCommand::WorkspaceStart { hub_url: Some(url), cwd },
            } if request_id == "ws-1"
                && url == "wss://hub.example/v1/tools"
                && cwd == "/home/u/proj"
        ));
    }

    #[test]
    fn workspace_status_payload_roundtrip() {
        let payload = ControlPayload::WorkspaceStatus {
            state: "running".into(),
            hub_url: Some("wss://hub.example/v1/tools".into()),
            cwd: Some("/home/u/proj".into()),
            uptime_ms: 4200,
            active_tool_calls: 2,
            sessions: vec!["echo-agent-a".into(), "echo-agent-b".into()],
            pid: 4242,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: ControlPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, payload);
        assert!(json.contains("\"type\":\"workspace_status\""));
    }

    #[test]
    fn workspace_status_payload_defaults_optional_fields() {
        let json = r#"{"type":"workspace_status","state":"none","uptime_ms":0,"active_tool_calls":0,"pid":1}"#;
        let decoded: ControlPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(
            decoded,
            ControlPayload::WorkspaceStatus {
                state,
                hub_url: None,
                cwd: None,
                sessions,
                ..
            } if state == "none" && sessions.is_empty()
        ));
    }

    #[test]
    fn workspace_exposure_capability_defaults_false() {
        let json = r#"{"control_v1":true,"runtime_cpu_profile":false,"profile_formats":[]}"#;
        let caps: LeaderCapabilities = serde_json::from_str(json).unwrap();
        assert!(!caps.workspace_exposure);
    }

    #[test]
    fn client_id_is_unique() {
        let ids: Vec<_> = (0..100).map(|_| ClientId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().map(|c| c.0).collect();
        assert_eq!(unique.len(), 100);
    }

    // --- ShuttingDown / ShutdownReason tests ---

    #[tokio::test]
    async fn shutting_down_message_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let msg = ServerMessage::ShuttingDown {
            reason: ShutdownReason::AutoUpdate,
            delay_ms: 2000,
        };

        write_message(&mut client, &msg).await.unwrap();
        let received: ServerMessage = read_message(&mut server).await.unwrap();

        match received {
            ServerMessage::ShuttingDown { reason, delay_ms } => {
                assert_eq!(reason, ShutdownReason::AutoUpdate);
                assert_eq!(delay_ms, 2000);
            }
            _ => panic!("Expected ShuttingDown, got {:?}", received),
        }
    }

    #[test]
    fn every_internal_method_carries_the_routable_prefix() {
        use strum::IntoEnumIterator;

        for method in InternalMethod::iter() {
            for line in [
                internal_notification(method, serde_json::json!({})),
                internal_request_line("id", method, serde_json::json!({})),
            ] {
                let json: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
                assert_eq!(
                    json["method"].as_str().and_then(|m| m.strip_prefix('_')),
                    Some(method.name()),
                    "unroutable wire method: {line}"
                );
            }
        }
    }

    #[test]
    fn shutdown_reason_variants_serialize_correctly() {
        let auto = serde_json::to_string(&ShutdownReason::AutoUpdate).unwrap();
        assert_eq!(auto, "\"auto_update\"");

        let idle = serde_json::to_string(&ShutdownReason::IdleTimeout).unwrap();
        assert_eq!(idle, "\"idle_timeout\"");

        let manual = serde_json::to_string(&ShutdownReason::Manual).unwrap();
        assert_eq!(manual, "\"manual\"");

        // Verify deserialization
        let parsed: ShutdownReason = serde_json::from_str("\"auto_update\"").unwrap();
        assert_eq!(parsed, ShutdownReason::AutoUpdate);
    }
}
