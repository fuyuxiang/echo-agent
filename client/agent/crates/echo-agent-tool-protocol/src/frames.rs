//! Per-method `params` and `result` payload structs.
//!
//! These types ride inside a [`crate::JsonRpcRequest`] /
//! [`crate::JsonRpcResponse`] / [`crate::JsonRpcNotification`].
//!
//! `session_id` belongs in the JSON-RPC envelope field — always.
//! Request/notification params structs do NOT carry a `session_id`;
//! the hub reads it from `request.session_id` on the envelope.
//! Types that are NOT request params (e.g. `ToolsChanged` notification
//! body, `ServerInfo` display struct) keep their own `session_id`
//! because it is payload data, not routing.

use serde::{Deserialize, Serialize};

use crate::{
    ConnectionId, FrameSeq, HookEvent, ServerId, SessionId, ToolCallId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, notification_wire::WireToolNotification,
    output_wire::ToolOutputWire,
};

// ── Tool call params / result / progress ─────────────────────────────────

/// `tool.call` (harness → service) and `tool_call_request` (service →
/// tool_server) share the same params shape; `tool_call_id` is preserved
/// end-to-end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub tool_call_id: ToolCallId,
    pub tool_id: ToolId,
    pub arguments: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_version: Option<String>,
    /// OS-native path; informational for cross-FS tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// W3C `traceparent` for distributed tracing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<String>,
}

/// Body of a successful `tool_call_result` response.
///
/// `follow_ups` and `reminders` only fire for **local** tools and are
/// empty (and field-skipped) for remote calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub tool_call_id: ToolCallId,
    pub output: ToolOutputWire,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<serde_json::Value>,
    /// Carried as opaque `Value` (not the runtime's typed frame) because
    /// this crate must not depend on `echo-agent-tool-runtime`. Sampler-side wire
    /// decoders reconstruct it into the typed frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_completion_output: Option<serde_json::Value>,
}

// ── Trace donation ────────────────────────────────────────────────────────

/// Hub rejects oversized batches wholesale; donors chunk before encoding.
pub const MAX_SPANS_PER_DONATION: usize = 512;

/// Maximum decoded `ExportTraceServiceRequest` size the hub accepts.
pub const MAX_DONATION_BYTES: usize = 1024 * 1024;

/// `traces.donate` params (tool_server → service notification).
/// Envelope `session_id` required. `hub.*` span attributes are
/// reserved — the hub strips them and stamps its own attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracesDonateParams {
    /// Base64 (standard alphabet, padded) protobuf-encoded
    /// `opentelemetry.proto.collector.trace.v1.ExportTraceServiceRequest`.
    pub otlp_request: String,
}

// ── Log donation ──────────────────────────────────────────────────────────

/// Hub rejects oversized batches wholesale; donors chunk before encoding.
/// The 1 MiB [`MAX_DONATION_BYTES`] decoded-size cap is the real bound; this
/// record cap is a secondary guard symmetric with [`MAX_SPANS_PER_DONATION`].
pub const MAX_LOG_RECORDS_PER_DONATION: usize = 512;

/// `logs.donate` params (tool_server → service notification).
/// Envelope `session_id` required. `hub.*` log attributes are reserved —
/// the hub strips them and stamps its own attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsDonateParams {
    /// Base64 (standard alphabet, padded) protobuf-encoded
    /// `opentelemetry.proto.collector.logs.v1.ExportLogsServiceRequest`.
    pub otlp_request: String,
}

// ── Metric donation ───────────────────────────────────────────────────────

/// Hub rejects oversized batches wholesale; donors chunk before encoding.
/// Secondary guard alongside the 1 MiB [`MAX_DONATION_BYTES`] decoded-size cap.
pub const MAX_METRICS_PER_DONATION: usize = 512;

/// `metrics.donate` params (tool_server → service notification).
/// **No envelope `session_id`** — metrics are process-aggregate, not
/// per-session (unlike [`LogsDonateParams`]). `hub.*` resource attributes
/// are reserved — the hub strips them and stamps its own attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsDonateParams {
    /// Base64 (standard alphabet, padded) protobuf-encoded
    /// `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest`.
    pub otlp_request: String,
}

/// Body of a `tool_call_progress` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallProgressFrame {
    pub tool_call_id: ToolCallId,
    /// Producer-defined kind, e.g. `"log_chunk"` or `"chunk"`.
    pub kind: String,
    pub body: serde_json::Value,
    /// Drop-bookkeeping counter — non-zero when prior progress frames for
    /// this `tool_call_id` were dropped under rate pressure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u32>,
}

/// Body of a `tool.notification` frame.
///
/// Both the harness and the tool server may emit notifications; `tool_id`
/// is omitted when the producer is the harness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolNotificationFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<ToolId>,
    pub notification: WireToolNotification,
}

impl ToolNotificationFrame {
    /// Build a custom notification with an application-defined `kind` and free-form payload.
    pub fn custom(tool_id: ToolId, kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            tool_call_id: None,
            tool_id: Some(tool_id),
            notification: WireToolNotification::Custom(
                crate::notification_wire::WireCustomNotification {
                    kind: kind.into(),
                    payload,
                },
            ),
        }
    }

    /// Build a notification wrapping a typed ("known") notification value.
    ///
    /// The `notification` is serialized into the `WireToolNotification::Known`
    /// variant. Returns `Err` only if serialization fails, which should not
    /// happen for well-formed `Serialize` types.
    pub fn known<N: Serialize>(
        tool_id: ToolId,
        notification: N,
    ) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(notification)?;
        Ok(Self {
            tool_call_id: None,
            tool_id: Some(tool_id),
            notification: WireToolNotification::Known(value),
        })
    }
}

/// Maximum serialized size of a `system.notify` opaque payload.
pub const MAX_SYSTEM_NOTIFY_PAYLOAD_BYTES: usize = 256 * 1024;

/// Body of a `system.notify` frame. `payload` is an opaque `SystemNotification`
/// JSON value forwarded verbatim without decoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemNotifyParams {
    pub payload: serde_json::Value,
    /// Target conversation override; defaults to the envelope `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id_override: Option<String>,
    /// Ask the gateway to echo the payload to WS subscribers.
    #[serde(default)]
    pub echo_to_subscribers: bool,
    /// Correlation id echoed back in the ack/response; not an idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

// ── Registration frames ──────────────────────────────────────────────────

/// `register_tool` params — single-tool sugar over `register_server`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterToolParams {
    pub tool: ToolRegistration,
}

/// `register_server` params — multi-tool batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterServerParams {
    pub server: ToolServerRegistration,
}

/// Drop a tool entirely from the connection (across every session it was
/// bound to). For per-session removal use
/// [`crate::Method::UnbindToolSession`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnregisterToolParams {
    pub tool_id: ToolId,
}

/// Drop every tool registered under `server_id` by this connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnregisterServerParams {
    pub server_id: ServerId,
}

// ── Per-tool session binding ───────────────────────────────────────────────

/// `bind_tool_session` params — add `session_id` to a registered tool's
/// per-tool session set.
///
/// Both fields are SUBJECTS of the operation: `tool_id` names the
/// tool whose session set is being mutated, and `session_id` names
/// the session being added. Neither matches the envelope-level
/// `session_id`, which is the calling-frame routing scope and is
/// typically omitted on connection-control frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindToolSessionParams {
    /// The tool whose session set is being mutated.
    pub tool_id: ToolId,
    /// The session id to add to the tool's session set. Must already
    /// be in the connection's bound-session set.
    pub session_id: SessionId,
}

/// Outcome reported by [`BindToolSessionAck`].
///
/// This is intentionally a **strict subset** of the registry-side
/// `echo_agent_computer_hub_core::registry::ToolSessionBindOutcome`, with one
/// extra wire-only variant. The asymmetry exists because the wire and
/// registry layers have different failure vocabularies:
///
/// - The registry's `Conflict` variant (cross-connection race on the
///   `(session_id, tool_id)` reverse-index slot) is NOT mirrored here.
///   The router lifts `Conflict` into a top-level
///   `ServerError::ToolBindingConflict` (-32600) so the contended caller
///   sees a wire-level error frame with a dedicated code instead of a
///   quietly-buried ack outcome — mirroring it would re-introduce the
///   `UnknownTool`-overload ambiguity the dedicated code was added to fix.
/// - `SessionNotBound` is router-injected by the per-frame envelope
///   pre-check (the connection's bound-session set is router state, not
///   registry state) and never originates from the registry call.
///
/// Adding a new variant here means adding a corresponding registry-side
/// outcome OR documenting why the wire-only variant is router-injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSessionBindOutcome {
    /// Added to the tool's session set.
    Bound,
    /// Already in the session set; no-op.
    AlreadyBound,
    /// No tool with this id is registered against the calling connection.
    UnknownTool,
    /// `session_id` is not in the connection's bound-session set; the caller
    /// must `register_session` first.
    SessionNotBound,
}

/// Reply to [`BindToolSessionParams`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindToolSessionAck {
    pub outcome: ToolSessionBindOutcome,
}

/// `unbind_tool_session` params — drop `session_id` from a registered
/// tool's per-tool session set.
///
/// Same envelope-vs-payload distinction as
/// [`BindToolSessionParams`]: `tool_id` and `session_id` are subjects
/// of the unbind operation; the envelope `session_id` (if present) is
/// the calling-frame routing scope and serves a different concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnbindToolSessionParams {
    /// The tool whose session set is being mutated.
    pub tool_id: ToolId,
    /// The session id to remove from the tool's session set.
    pub session_id: SessionId,
}

/// Outcome reported by [`UnbindToolSessionAck`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSessionUnbindOutcome {
    /// Removed from the tool's session set.
    Unbound,
    /// Was not in the tool's session set; no-op.
    NotBound,
    /// No tool with this id is registered against the calling connection.
    UnknownTool,
}

/// Reply to [`UnbindToolSessionParams`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnbindToolSessionAck {
    pub outcome: ToolSessionUnbindOutcome,
}

// ── Server discovery + binding ────────────────────────────────────────────

/// `servers.list` params — discover available tool servers for the
/// authenticated user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServersListParams {}

/// Metadata about a connected tool server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub server_id: ServerId,
    /// The tool server's own session ID (used internally for routing).
    /// May be absent when the hub omits it from `servers.list` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub connected_since: String,
    /// Lifecycle status (Ready, Busy, Draining, etc.).
    pub status: ToolServerLifecycleStatus,
}

/// Reply to [`ServersListParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServersListResult {
    pub servers: Vec<ServerInfo>,
}

/// `server.bind` params — bind a tool server's tools to a harness session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerBindParams {
    /// Which tool server to bind (its server_id from `servers.list`).
    pub server_id: ServerId,
    /// The harness session to bind tools to.
    pub session_id: SessionId,
}

/// Outcome of a `server.bind` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerBindOutcome {
    /// Tools successfully bound to the session.
    Bound,
    /// Tools were already bound to this session.
    AlreadyBound,
    /// No server with this server_id found.
    ServerNotFound,
    /// A server was located and the bind forwarded, but it did not complete:
    /// the ack timed out, the transport send/delivery failed, or the ack was
    /// malformed or an explicit error. Distinct from `ServerNotFound`, which
    /// means no such server is registered.
    Unavailable,
}

/// Reply to [`ServerBindParams`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerBindAck {
    pub outcome: ServerBindOutcome,
}

/// `server.unbind` params — unbind a tool server's tools from a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerUnbindParams {
    pub server_id: ServerId,
    pub session_id: SessionId,
}

/// Outcome of a `server.unbind` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerUnbindOutcome {
    Unbound,
    ServerNotFound,
}

/// Reply to [`ServerUnbindParams`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerUnbindAck {
    pub outcome: ServerUnbindOutcome,
}

// ── List & search ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsListParams {
    pub session_id: SessionId,
    pub mode: ToolDefinitionMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<echo_agent_tool_types::ToolDescription>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsSearchParams {
    pub session_id: SessionId,
    pub query: String,
    pub limit: usize,
}

/// One match in a `tools.search_result` body. Wire-local definition keeps
/// the protocol crate free of the codegen crate's transitive deps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSearchResult {
    pub tool_name: String,
    pub server_name: String,
    pub description: String,
    pub score: f32,
    pub parameters: Vec<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsSearchResultBody {
    pub results: Vec<ToolSearchResult>,
    pub total_hidden_tools: usize,
    pub is_ready: bool,
}

// ── Session lifecycle ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOpenParams {
    /// `true` when reconnecting an existing session within the grace
    /// window; requires `last_seq` to dedup notifications.
    #[serde(default)]
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<LastSeq>,
}

/// Reconnect helper: the last `(connection_id, seq)` the client saw
/// before disconnecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSeq {
    pub connection_id: ConnectionId,
    pub seq: FrameSeq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCloseParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Reply to a `session_open` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionOpenResult {}

/// `session_bind_server` params (harness → hub). Bind a tool server's
/// tools to the current session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBindServerParams {
    pub server_id: ServerId,
    /// Working directory for the session. The tool server creates a
    /// session rooted at this path. When absent, the server's default
    /// CWD is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Opaque metadata passed through to the tool server (sandbox_id,
    /// agent config, user preferences). The hub does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Reply to [`SessionBindServerParams`]. Returns the tools available
/// after the server is bound.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindServerResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<echo_agent_tool_types::ToolDescription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<String>,
    /// [`SessionBindResult::unserved_tool_ids`], forwarded verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unserved_tool_ids: Vec<String>,
    /// [`SessionBindResult::resolve_error`], forwarded verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_error: Option<String>,
    /// [`SessionBindResult::image_capabilities`], forwarded verbatim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_capabilities: Vec<String>,
}

/// `session_unbind_server` params (harness → hub). Unbind a tool
/// server from the current session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUnbindServerParams {
    pub server_id: ServerId,
}

/// `session_attach_server` params (harness → hub). Attach this harness
/// connection to an EXISTING session as an observer: verify a tool-server
/// is bound for the envelope session and return the tool snapshot.
/// Hub-local: never forwarded to the tool server; never creates a
/// workspace session; never mutates toolsets/handlers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionAttachServerParams {
    /// Optional expected server (diagnostics + directory cross-check);
    /// the authoritative key is the envelope `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<ServerId>,
    /// Free-form caller label for metrics/logs ("fs_read", "deploy_app", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

/// Reply to [`SessionAttachServerParams`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionAttachServerResult {
    /// Registry snapshot for the session (local + cross-instance), same
    /// shape as [`SessionBindServerResult::tools`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<echo_agent_tool_types::ToolDescription>,
    /// Where the session's tool-server was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<AttachRoute>,
}

/// Where an attach found the session's tool-server. `Unknown` absorbs
/// values a newer peer may add, so the typed parse never fails across
/// independently-deployed hub/SDK versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachRoute {
    /// Live tool-server connection on the answering hub instance.
    Local,
    /// Published cross-instance routes (tool-server on another replica).
    Remote,
    #[serde(other)]
    Unknown,
}

// ── Simplified lifecycle ─────────────────────────────────────────────────

/// `serve` params (server → hub). Full tool snapshot for a session.
///
/// Idempotent: re-sending replaces the tool set for the envelope
/// `session_id`. The hub diffs against the previous snapshot and
/// emits `tools_changed` to subscribed harnesses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServeParams {
    pub tools: Vec<crate::ToolDescriptionWithSchema>,
}

/// Reply to [`ServeParams`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServeResult {
    /// Number of tools accepted (informational).
    #[serde(default)]
    pub accepted: usize,
    /// Tool IDs that were added relative to the previous snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ToolId>,
    /// Tool IDs that were removed relative to the previous snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ToolId>,
}

/// `session.bind` params (hub → server). Hub requests the server to
/// start serving a session.
///
/// The session id is carried on the JSON-RPC envelope, not in params.
/// The server responds with its tool snapshot (via the JSON-RPC
/// response). On reconnect the server replays `serve{tools}` for
/// every remembered session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBindParams {}

/// Reply to [`SessionBindParams`]. The server's tool snapshot for the
/// newly bound session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionBindResult {
    pub tools: Vec<echo_agent_tool_types::ToolDescription>,
    /// Version of the responding tool-server binary. `None` on servers
    /// predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<String>,
    /// Pinned tool ids this binary could not serve (unknown to its registry).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unserved_tool_ids: Vec<String>,
    /// Reason the server failed the toolset resolution closed (the bind then
    /// advertises no model-facing tools by design). `None` on normal
    /// resolutions and on servers predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_error: Option<String>,
    /// Advisory image capability tokens declared by the image the responding
    /// server runs in, sorted, deduped and validated by that server. Empty
    /// both on servers predating the field and on images predating the
    /// declaration; [`IMAGE_CAPABILITIES_V1`] tells the two apart, not this
    /// field's shape.
    ///
    /// Set membership only, never ordered comparison, and never an
    /// authorization input — the tokens are guest-forgeable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_capabilities: Vec<String>,
}

/// Self-describing image capability token. Its presence in
/// [`SessionBindResult::image_capabilities`] means the set is authoritative
/// (a token not in it is genuinely absent); its absence means unknown.
pub const IMAGE_CAPABILITIES_V1: &str = "capabilities.v1";

/// Cap on the number of tokens in [`SessionBindResult::image_capabilities`].
pub const MAX_IMAGE_CAPABILITIES: usize = 128;
/// Cap on one token's length in bytes.
pub const MAX_IMAGE_CAPABILITY_LEN: usize = 64;

/// Charset/length gate for an image capability token: dot-separated
/// `[a-z0-9-]` segments, at least two of them, no empty segment, no
/// leading/trailing dash, at most [`MAX_IMAGE_CAPABILITY_LEN`] bytes. So it
/// rejects dotfiles, `..`, uppercase and overlong names.
///
/// Declared once here, beside [`IMAGE_CAPABILITIES_V1`], because the producing
/// reader and every consumer that re-validates the guest-forgeable set must
/// apply identical rules: a copy that drifts stricter silently drops tokens the
/// image legitimately declared.
pub fn is_image_capability_token(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_IMAGE_CAPABILITY_LEN {
        return false;
    }
    let mut segments = 0usize;
    for segment in name.split('.') {
        let bytes = segment.as_bytes();
        let charset_ok = bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if bytes.is_empty() || !charset_ok || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

/// `session.unbind` params (hub → server). Hub tells the server to
/// stop serving a session. Sent as a JSON-RPC **notification** (no
/// `id` field) — the hub does not expect or wait for a response.
///
/// The session id is carried on the JSON-RPC envelope, not in params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUnbindParams {}

// ── Subscriptions ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeNotificationsParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<NotificationFilter>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<ToolId>,
    /// Whitelist of accepted notification `kinds`. `None` means accept
    /// all kinds; an empty `Vec` means accept none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
}

/// Reply outcome reported by [`SubscribeAck`].
///
/// `Subscribed` and `AlreadySubscribed` discriminate first-time binds
/// from idempotent retries; `NotAuthorized` is reserved for the case
/// where the subscriber's connection has not bound `session_id` via
/// `register_session` — the same precondition that gates dispatch and
/// hook frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeOutcome {
    /// Subscription was newly added to the session's subscriber set.
    Subscribed,
    /// Subscription was already present; no-op.
    AlreadySubscribed,
    /// Subscriber's connection has not bound `session_id`; the request
    /// is rejected without mutating any state.
    NotAuthorized,
}

/// Reply to [`SubscribeNotificationsParams`].
///
/// `subscription_id` is the harness-facing handle that callers thread
/// through subsequent [`UnsubscribeNotificationsParams`] requests; the
/// service uses `(connection_id, session_id)` internally so the id is
/// informational and a single value (`"default"`) is reused per
/// `(connection, session)` pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeAck {
    pub outcome: SubscribeOutcome,
    pub subscription_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnsubscribeNotificationsParams {
    pub session_id: SessionId,
    pub subscription_id: String,
}

/// Reply outcome reported by [`UnsubscribeAck`].
///
/// `Evicted` is server-pushed: the service emits an unsubscribe ack
/// with this outcome to a subscriber whose mpsc was dropped during
/// fan-out (see slow-consumer eviction in the computer hub crate).
/// Clients reading that frame on a connection they did not initiate
/// an unsubscribe on should treat the subscription as gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsubscribeOutcome {
    /// Subscription was present and was removed.
    Unsubscribed,
    /// Subscription was not present; no-op.
    NotSubscribed,
    /// Subscription was removed by the service because the subscriber's
    /// outbound mpsc was full or dropped during fan-out.
    Evicted,
}

/// Reply to [`UnsubscribeNotificationsParams`] and the service-pushed
/// frame for slow-consumer eviction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnsubscribeAck {
    pub outcome: UnsubscribeOutcome,
    pub subscription_id: String,
}

// ── Hooks ────────────────────────────────────────────────────────────────

/// `hook` frame body, routed in both directions through the hub: harness →
/// tool-server for forward hooks (e.g. `Cancel`, `SessionEnded`), and
/// tool-server → harness for reverse request/response hooks (e.g. permission
/// requests).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookFrame {
    pub session_id: SessionId,
    /// Omit for session-wide hooks (broadcast); required for call-scoped
    /// hooks like `Cancel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<ToolId>,
    /// Required for call-scoped hooks (`Cancel`); optional otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ToolCallId>,
    /// Correlation id for a request/response hook. The requester mints it and
    /// the responder echoes it in the [`HookReplyFrame`], so the reply routes
    /// back to the awaiting caller. Set in both directions (harness ↔
    /// tool-server). `None` for fire-and-forget hooks such as `Cancel` and
    /// `SessionEnded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<String>,
    pub event: HookEvent,
    /// W3C `traceparent` (mirrors [`ToolCallParams::trace_context`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<String>,
}

impl HookFrame {
    /// Build a `Cancel` hook targeting a specific in-flight call.
    pub fn cancel(session_id: SessionId, tool_id: ToolId, call_id: ToolCallId) -> Self {
        Self {
            session_id,
            tool_id: Some(tool_id),
            call_id: Some(call_id),
            hook_id: None,
            event: HookEvent::Cancel,
            trace_context: None,
        }
    }

    /// Build a `Pause` hook broadcast to every tool server bound to the session.
    pub fn pause(session_id: SessionId) -> Self {
        Self {
            session_id,
            tool_id: None,
            call_id: None,
            hook_id: None,
            event: HookEvent::Pause,
            trace_context: None,
        }
    }

    /// Build a `Resume` hook broadcast to every tool server bound to the session.
    pub fn resume(session_id: SessionId) -> Self {
        Self {
            session_id,
            tool_id: None,
            call_id: None,
            hook_id: None,
            event: HookEvent::Resume,
            trace_context: None,
        }
    }

    /// Build a `SessionEnded` hook broadcast to every tool server bound to the session.
    pub fn session_ended(session_id: SessionId) -> Self {
        Self {
            session_id,
            tool_id: None,
            call_id: None,
            hook_id: None,
            event: HookEvent::SessionEnded,
            trace_context: None,
        }
    }

    /// Build a `Custom` hook with an application-defined `kind` and free-form payload.
    pub fn custom(session_id: SessionId, kind: String, payload: serde_json::Value) -> Self {
        Self {
            session_id,
            tool_id: None,
            call_id: None,
            hook_id: None,
            event: HookEvent::Custom { kind, payload },
            trace_context: None,
        }
    }

    /// Session-scoped request/response `Custom` hook that sets `hook_id` so the hub wires a [`HookReplyFrame`] reply leg.
    pub fn custom_request(
        session_id: SessionId,
        hook_id: String,
        kind: String,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            session_id,
            tool_id: None,
            call_id: None,
            hook_id: Some(hook_id),
            event: HookEvent::Custom { kind, payload },
            trace_context: None,
        }
    }

    /// Attach a W3C `traceparent` (builder-style).
    pub fn with_trace_context(mut self, trace_context: Option<String>) -> Self {
        self.trace_context = trace_context;
        self
    }
}

/// `hook_reply` frame body: reply to a request/response [`HookFrame`], correlated by `hook_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookReplyFrame {
    pub session_id: SessionId,
    /// Echoed from the originating [`HookFrame::hook_id`].
    pub hook_id: String,
    pub result: serde_json::Value,
}

// ── Service → harness pushes ─────────────────────────────────────────────

/// `tools_changed` body — the active tool set for `session_id` changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsChanged {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ToolId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<ToolId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated: Vec<ToolId>,
}

// ── Tool server status lifecycle ──────────────────────────────────────

/// Lifecycle status of a tool server connection.
///
/// `starting` → `ready` → `busy` ↔ `ready` → `draining` → `shutting_down`.
/// `disconnected` is hub-only: set during disconnect cleanup, never sent
/// by the tool server itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolServerLifecycleStatus {
    Starting,
    #[default]
    Ready,
    Busy,
    Draining,
    ShuttingDown,
    Disconnected,
}

/// `tool_server.status` payload. `session_id = Some` scopes counters
/// to that session; `None` is an aggregate across all sessions. Newer
/// fields use `#[serde(default)]` for forward/backward wire compat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolServerStatusPayload {
    pub status: ToolServerLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Dead connection on Disconnected (remote hubs scope in-flight cancel).
    ///
    /// A raw `String`, not a typed `ConnectionId`: a malformed id degrades
    /// leniently on the consumer (re-parsed, logged, ignored) instead of failing
    /// deserialization of the whole status frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    pub active_tool_calls: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tool_names: Vec<String>,
    pub background_tasks: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background_task_ids: Vec<String>,
    pub pending_tool_calls: u32,
    pub last_tool_call_started_ms: u64,
    pub last_tool_call_completed_ms: u64,
    pub uptime_ms: u64,
    /// `None` while busy; epoch ms of the last busy→ready transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since_ms: Option<u64>,
    /// Items accepted by the durable upload queue but not yet uploaded
    /// (includes `upload_queue_inflight`). `0` when no queue is configured.
    #[serde(default)]
    pub upload_queue_pending: u32,
    /// Total bytes of the pending upload-queue spill files on disk.
    #[serde(default)]
    pub upload_queue_pending_bytes: u64,
    /// Pending items the worker is actively uploading right now (a subset of
    /// `upload_queue_pending`).
    #[serde(default)]
    pub upload_queue_inflight: u32,
    /// `true` while the upload queue's circuit breaker is paused on a run of
    /// transient upload failures.
    #[serde(default)]
    pub upload_queue_circuit_breaker_tripped: bool,
    /// Detached artifact-producer tasks (archive build, tool_state, tool
    /// definitions) still running — work not yet handed to the upload queue.
    #[serde(default)]
    pub artifact_producers_inflight: u32,
    /// Epoch ms when a graceful drain began (SIGTERM or hub evict); `None`
    /// until draining starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_started_ms: Option<u64>,
    /// Whether a turn is currently active. Per-session for a session-scoped
    /// snapshot; "any session has an active turn" for the aggregate.
    #[serde(default)]
    pub turn_active: bool,
    /// `true` when the reported idle verdict was computed ignoring background
    /// tasks.
    #[serde(default)]
    pub idle_ignores_background: bool,
    /// Why `idle_since_ms` is being withheld, when it is. `None` ⇒ not withheld
    /// (or the tool server is genuinely busy, which `active_tool_calls`
    /// reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withhold_reason: Option<IdleWithholdReason>,
    /// Epoch ms the current hold is measured from. Never `None` while
    /// `withhold_reason` is `Some`.
    ///
    /// NOT the instant the withhold began, for the preview reasons. It is the
    /// **real-use anchor**: the last routed request or tool call, floored at
    /// process start. A status poll never moves it, which is the point — a
    /// ceiling has to be measured from genuine use, or the pane could hold a
    /// sandbox open forever by resetting the clock it is judged against. So
    /// for a poll-pinned session this is *older* than the poll-only period,
    /// and `now - withhold_since_ms` is time-since-real-use, not
    /// time-spent-withholding. `Durability` is the exception: there it is the
    /// true busy-since stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withhold_since_ms: Option<u64>,
    /// `true` once the current hold has crossed its effective ceiling. The
    /// verdict is published rather than re-derived because the ceiling is
    /// per-session config only the sender can see. Always `false` today: no
    /// ceilings are configured yet.
    #[serde(default)]
    pub withhold_capped: bool,
    /// Open WebSocket (HMR) tunnels through the in-sandbox preview proxy.
    /// Nonzero ⇒ a client is attached even if the preview is otherwise silent.
    #[serde(default)]
    pub preview_ws_tunnels_open: u32,
}

/// Why a tool server is withholding `idle_since_ms`, ordered by strength of
/// evidence that someone is really using the sandbox.
///
/// Carries an [`Unknown`](Self::Unknown) escape so adding a variant cannot
/// break older readers: without it, one unrecognised string would fail
/// deserialization of the **entire** status frame, taking the idle verdict down
/// with it. Sandbox binaries are pinned per session, so old and new report side
/// by side for at least a full session TTL.
///
/// Deliberately not `#[non_exhaustive]`: in-workspace matches should stay
/// exhaustive so a new variant is a compile error at every decision site, which
/// is a different problem from wire tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleWithholdReason {
    /// Artifact producers or queued uploads outstanding. Already bounded by the
    /// durability idle-hold cap.
    Durability,
    /// An open WebSocket tunnel or a routed request in flight. Never a status
    /// poll, however long it is held.
    PreviewAttached,
    /// Recent `Routed` preview traffic — a human loading the app.
    PreviewRouted,
    /// Only the preview pane's own `/__echo_agent-preview/status` liveness poll.
    PreviewStatusOnly,
    /// A recent client-driven mutation RPC (file write, git commit, …) — a
    /// human working on the workspace through its RPC surface rather than
    /// through agent tool calls or the preview.
    ClientRpc,
    /// A recent `workspace.presence.note`. Like
    /// [`PreviewStatusOnly`](Self::PreviewStatusOnly) it never advances the
    /// withhold anchor, so the hub's hold ceiling genuinely caps it.
    ClientPresence,
    /// A live scheduled task (`/loop`) has a run coming soon; the workspace limits this hold itself.
    ScheduledTask,
    /// A reason this build does not recognise — a newer sender. Never
    /// constructed locally; only produced by deserialization.
    #[serde(other)]
    Unknown,
}

impl ToolServerStatusPayload {
    /// Zeroed-out payload for terminal states (Disconnected, Starting, etc.).
    pub fn terminal(status: ToolServerLifecycleStatus) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }
}

/// `tool_server.evict` params — hub requests graceful shutdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServerEvictParams {
    pub session_id: SessionId,
    pub reason: String,
    /// Deadline (ms) before the hub force-closes the connection.
    pub grace_period_ms: u64,
}

/// `tool_server.get_status` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolServerGetStatusParams {
    pub session_id: SessionId,
}

/// One entry in [`ToolServerGetStatusResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServerConnectionStatus {
    pub connection_id: String,
    pub status: ToolServerStatusPayload,
}

/// Reply to `tool_server.get_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServerGetStatusResult {
    pub tool_servers: Vec<ToolServerConnectionStatus>,
}

/// Why a tool server connection was dropped (carried in the hub's
/// `tool_server.status_changed` disconnect notification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolServerDisconnectReason {
    NormalClose,
    IdleTimeout,
    ForceEvicted,
    ConnectionLost,
}

// ── Heartbeat ────────────────────────────────────────────────────────────
//
// PingFrame / PongFrame carry a `method` discriminator on the wire so
// any receiver (hub or SDK) can route them through a method-based demux.
// The `method` value is baked into the Serialize impl — callers just set
// `ts_ms` and the correct method string appears in the JSON output.
//
// Deserialization is lenient: the `method` field is accepted but ignored,
// so frames produced by older builds (without `method`) still parse.

/// Application-level heartbeat ping.
///
/// Serializes as `{"method":"ping","ts_ms":<u64>}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingFrame {
    pub ts_ms: u64,
}

/// Application-level heartbeat pong.
///
/// Serializes as `{"method":"pong","ts_ms":<u64>}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PongFrame {
    pub ts_ms: u64,
}

impl PingFrame {
    pub fn new(ts_ms: u64) -> Self {
        Self { ts_ms }
    }
}

impl PongFrame {
    pub fn new(ts_ms: u64) -> Self {
        Self { ts_ms }
    }
}

// -- Custom Serialize: always includes `"method"` on the wire. -----------

impl serde::Serialize for PingFrame {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("method", crate::methods::Method::Ping.as_wire_str())?;
        map.serialize_entry("ts_ms", &self.ts_ms)?;
        map.end()
    }
}

impl serde::Serialize for PongFrame {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("method", crate::methods::Method::Pong.as_wire_str())?;
        map.serialize_entry("ts_ms", &self.ts_ms)?;
        map.end()
    }
}

// -- Custom Deserialize: accepts with or without `method` for compat. ----

impl<'de> serde::Deserialize<'de> for PingFrame {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            method: Option<String>,
            ts_ms: u64,
        }
        let raw = Raw::deserialize(d)?;
        if let Some(ref m) = raw.method
            && m != crate::methods::Method::Ping.as_wire_str()
        {
            return Err(serde::de::Error::custom(format!(
                "expected method \"{}\" but got \"{m}\"",
                crate::methods::Method::Ping.as_wire_str(),
            )));
        }
        Ok(Self { ts_ms: raw.ts_ms })
    }
}

impl<'de> serde::Deserialize<'de> for PongFrame {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            method: Option<String>,
            ts_ms: u64,
        }
        let raw = Raw::deserialize(d)?;
        if let Some(ref m) = raw.method
            && m != crate::methods::Method::Pong.as_wire_str()
        {
            return Err(serde::de::Error::custom(format!(
                "expected method \"{}\" but got \"{m}\"",
                crate::methods::Method::Pong.as_wire_str(),
            )));
        }
        Ok(Self { ts_ms: raw.ts_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sid() -> SessionId {
        SessionId::new("test-session").expect("valid")
    }

    fn tid() -> ToolId {
        ToolId::new("test-tool").expect("valid")
    }

    fn legacy_status_json() -> serde_json::Value {
        json!({
            "status": "ready",
            "active_tool_calls": 0,
            "background_tasks": 0,
            "pending_tool_calls": 0,
            "last_tool_call_started_ms": 0,
            "last_tool_call_completed_ms": 0,
            "uptime_ms": 0
        })
    }

    fn cid() -> ToolCallId {
        ToolCallId::new_v7()
    }

    // ── HookFrame constructors ──────────────────────────────────────

    #[test]
    fn hook_cancel_sets_tool_and_call_ids() {
        let hook = HookFrame::cancel(sid(), tid(), cid());
        assert_eq!(hook.event, HookEvent::Cancel);
        assert!(hook.tool_id.is_some());
        assert!(hook.call_id.is_some());
    }

    #[test]
    fn hook_pause_omits_tool_and_call_ids() {
        let hook = HookFrame::pause(sid());
        assert_eq!(hook.event, HookEvent::Pause);
        assert!(hook.tool_id.is_none());
        assert!(hook.call_id.is_none());
    }

    #[test]
    fn hook_resume_omits_tool_and_call_ids() {
        let hook = HookFrame::resume(sid());
        assert_eq!(hook.event, HookEvent::Resume);
        assert!(hook.tool_id.is_none());
        assert!(hook.call_id.is_none());
    }

    #[test]
    fn hook_session_ended_omits_tool_and_call_ids() {
        let hook = HookFrame::session_ended(sid());
        assert_eq!(hook.event, HookEvent::SessionEnded);
        assert!(hook.tool_id.is_none());
        assert!(hook.call_id.is_none());
    }

    #[test]
    fn hook_custom_carries_kind_and_payload() {
        let payload = json!({"key": "value"});
        let hook = HookFrame::custom(sid(), "my.hook".to_owned(), payload.clone());
        match &hook.event {
            HookEvent::Custom { kind, payload: p } => {
                assert_eq!(kind, "my.hook");
                assert_eq!(p, &payload);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn hook_cancel_round_trips_through_serde() {
        let hook = HookFrame::cancel(sid(), tid(), cid());
        let json = serde_json::to_value(&hook).expect("serialize");
        let back: HookFrame = serde_json::from_value(json).expect("deserialize");
        assert_eq!(hook, back);
    }

    // ── ToolNotificationFrame constructors ───────────────────────────

    #[test]
    fn notification_custom_sets_wire_shape() {
        let frame = ToolNotificationFrame::custom(tid(), "echo.status", json!({"status": "idle"}));
        assert_eq!(frame.tool_id.as_ref().unwrap(), &tid());
        assert!(frame.tool_call_id.is_none());
        match &frame.notification {
            WireToolNotification::Custom(c) => {
                assert_eq!(c.kind, "echo.status");
                assert_eq!(c.payload, json!({"status": "idle"}));
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn notification_known_serializes_value() {
        let inner = json!({"type": "BashOutputChunk", "data": "hello"});
        let frame = ToolNotificationFrame::known(tid(), inner.clone()).expect("serialize");
        match &frame.notification {
            WireToolNotification::Known(v) => assert_eq!(v, &inner),
            other => panic!("expected Known, got {other:?}"),
        }
    }

    #[test]
    fn system_notify_params_skips_optional_fields_when_none() {
        let params = super::SystemNotifyParams {
            payload: json!({"backgroundTaskCompleted": {"id": "t-1", "returnCode": 0}}),
            conversation_id_override: None,
            echo_to_subscribers: false,
            request_id: None,
        };
        let json = serde_json::to_value(&params).expect("serialize");
        assert!(json.get("conversation_id_override").is_none());
        assert!(json.get("request_id").is_none());
        assert_eq!(json["echo_to_subscribers"], false);
        let back: super::SystemNotifyParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    #[test]
    fn system_notify_params_echo_defaults_false_when_absent() {
        let json = json!({ "payload": {"k": "v"} });
        let back: super::SystemNotifyParams = serde_json::from_value(json).expect("deserialize");
        assert!(!back.echo_to_subscribers);
        assert!(back.conversation_id_override.is_none());
        assert!(back.request_id.is_none());
    }

    #[test]
    fn system_notify_params_round_trips_with_all_fields() {
        let params = super::SystemNotifyParams {
            payload: json!({"k": "v"}),
            conversation_id_override: Some("conv-7".into()),
            echo_to_subscribers: true,
            request_id: Some("bgtask-t-1".into()),
        };
        let json = serde_json::to_value(&params).expect("serialize");
        let back: super::SystemNotifyParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    // ── Donation params ────────────────────────────────────────────

    #[test]
    fn logs_donate_params_round_trips() {
        let params = super::LogsDonateParams {
            otlp_request: "b64payload".to_owned(),
        };
        let json = serde_json::to_value(&params).expect("serialize");
        assert_eq!(json["otlp_request"], "b64payload");
        let back: super::LogsDonateParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    #[test]
    fn metrics_donate_params_round_trips() {
        let params = super::MetricsDonateParams {
            otlp_request: "b64payload".to_owned(),
        };
        let json = serde_json::to_value(&params).expect("serialize");
        assert_eq!(json["otlp_request"], "b64payload");
        let back: super::MetricsDonateParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    // ── ToolServerStatusPayload ────────────────────────────────────

    #[test]
    fn tool_server_lifecycle_status_serde_snake_case() {
        let status = super::ToolServerLifecycleStatus::ShuttingDown;
        let json = serde_json::to_value(status).expect("serialize");
        assert_eq!(json.as_str(), Some("shutting_down"));
        let back: super::ToolServerLifecycleStatus =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, status);
    }

    #[test]
    fn tool_server_lifecycle_status_all_variants_round_trip() {
        use super::ToolServerLifecycleStatus::*;
        for (variant, expected_str) in [
            (Starting, "starting"),
            (Ready, "ready"),
            (Busy, "busy"),
            (Draining, "draining"),
            (ShuttingDown, "shutting_down"),
            (Disconnected, "disconnected"),
        ] {
            let json = serde_json::to_value(variant).expect("serialize");
            assert_eq!(json.as_str(), Some(expected_str), "variant {variant:?}");
            let back: super::ToolServerLifecycleStatus =
                serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn tool_server_status_payload_round_trips() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Busy,
            session_id: None,
            active_tool_calls: 2,
            active_tool_names: vec!["read_file".into(), "grep".into()],
            background_tasks: 1,
            background_task_ids: vec!["task-abc".into()],
            pending_tool_calls: 0,
            last_tool_call_started_ms: 1721234567890,
            last_tool_call_completed_ms: 1721234565000,
            uptime_ms: 342000,
            idle_since_ms: None,
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["status"], "busy");
        assert_eq!(json["active_tool_calls"], 2);
        assert!(
            json.get("idle_since_ms").is_none(),
            "None should be skipped"
        );
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, payload);
    }

    #[test]
    fn tool_server_status_payload_idle_since_present() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Ready,
            session_id: Some(sid()),
            active_tool_calls: 0,
            active_tool_names: vec![],
            background_tasks: 0,
            background_task_ids: vec![],
            pending_tool_calls: 0,
            last_tool_call_started_ms: 0,
            last_tool_call_completed_ms: 0,
            uptime_ms: 60000,
            idle_since_ms: Some(1721234560000),
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["idle_since_ms"], 1721234560000u64);
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.idle_since_ms, Some(1721234560000));
    }

    /// The six queue/drain fields round-trip with real values on the wire.
    #[test]
    fn tool_server_status_payload_carries_queue_and_drain_fields() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Draining,
            session_id: Some(sid()),
            connection_id: None,
            active_tool_calls: 1,
            active_tool_names: vec!["read_file".into()],
            background_tasks: 0,
            background_task_ids: vec![],
            pending_tool_calls: 0,
            last_tool_call_started_ms: 0,
            last_tool_call_completed_ms: 0,
            uptime_ms: 5000,
            idle_since_ms: None,
            upload_queue_pending: 7,
            upload_queue_pending_bytes: 4096,
            upload_queue_inflight: 2,
            upload_queue_circuit_breaker_tripped: true,
            artifact_producers_inflight: 3,
            drain_started_ms: Some(1721234599999),
            turn_active: true,
            idle_ignores_background: false,
            withhold_reason: None,
            withhold_since_ms: None,
            withhold_capped: false,
            preview_ws_tunnels_open: 0,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["upload_queue_pending"], 7);
        assert_eq!(json["upload_queue_pending_bytes"], 4096u64);
        assert_eq!(json["upload_queue_inflight"], 2);
        assert_eq!(json["upload_queue_circuit_breaker_tripped"], true);
        assert_eq!(json["artifact_producers_inflight"], 3);
        assert_eq!(json["drain_started_ms"], 1721234599999u64);
        assert_eq!(json["turn_active"], true);
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, payload);
    }

    /// `withhold_reason` is snake_case on the wire so it can be used directly
    /// as a metric label.
    #[test]
    fn tool_server_status_payload_carries_withhold_fields() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Ready,
            session_id: Some(sid()),
            idle_since_ms: None,
            withhold_reason: Some(super::IdleWithholdReason::PreviewStatusOnly),
            withhold_since_ms: Some(1721234560000),
            withhold_capped: true,
            preview_ws_tunnels_open: 2,
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["withhold_reason"], "preview_status_only");
        assert_eq!(json["withhold_since_ms"], 1721234560000u64);
        assert_eq!(json["withhold_capped"], true);
        assert_eq!(json["preview_ws_tunnels_open"], 2);
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, payload);
    }

    /// Every constructible reason keeps its snake_case wire string stable —
    /// these strings feed metric labels and cross-version peers directly.
    #[test]
    fn withhold_reason_wire_strings_are_stable() {
        // Exhaustive over the locally-constructible variants: a new variant
        // fails to compile here before it can reach the wire unpinned.
        for reason in [
            super::IdleWithholdReason::Durability,
            super::IdleWithholdReason::PreviewAttached,
            super::IdleWithholdReason::PreviewRouted,
            super::IdleWithholdReason::PreviewStatusOnly,
            super::IdleWithholdReason::ClientRpc,
            super::IdleWithholdReason::ClientPresence,
            super::IdleWithholdReason::ScheduledTask,
        ] {
            let expected = match reason {
                super::IdleWithholdReason::Durability => "durability",
                super::IdleWithholdReason::PreviewAttached => "preview_attached",
                super::IdleWithholdReason::PreviewRouted => "preview_routed",
                super::IdleWithholdReason::PreviewStatusOnly => "preview_status_only",
                super::IdleWithholdReason::ClientRpc => "client_rpc",
                super::IdleWithholdReason::ClientPresence => "client_presence",
                super::IdleWithholdReason::ScheduledTask => "scheduled_task",
                super::IdleWithholdReason::Unknown => unreachable!("never constructed locally"),
            };
            assert_eq!(
                serde_json::to_value(reason).expect("serialize"),
                serde_json::Value::String(expected.to_owned())
            );
            let back: super::IdleWithholdReason =
                serde_json::from_value(serde_json::Value::String(expected.to_owned()))
                    .expect("round-trip");
            assert_eq!(back, reason);
        }
    }

    /// An unrecognised reason from a newer sender must degrade to `Unknown`,
    /// not fail the whole frame and take the idle verdict with it.
    #[test]
    fn unknown_withhold_reason_does_not_fail_the_frame() {
        let json = serde_json::json!({
            "status": "ready",
            "active_tool_calls": 0,
            "background_tasks": 0,
            "pending_tool_calls": 0,
            "last_tool_call_started_ms": 0,
            "last_tool_call_completed_ms": 0,
            "uptime_ms": 1000,
            "withhold_reason": "some_future_reason",
            "withhold_since_ms": 1721234560000u64,
        });
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("a newer reason must not break the frame");
        assert_eq!(
            back.withhold_reason,
            Some(super::IdleWithholdReason::Unknown)
        );
        assert_eq!(
            back.withhold_since_ms,
            Some(1721234560000),
            "the rest of the frame must survive intact"
        );
    }

    /// The fleet is version-pinned per session, so old and new binaries report
    /// side by side for at least a full session TTL.
    #[test]
    fn tool_server_status_payload_withhold_fields_are_optional_on_the_wire() {
        let json = serde_json::json!({
            "status": "ready",
            "active_tool_calls": 0,
            "background_tasks": 0,
            "pending_tool_calls": 0,
            "last_tool_call_started_ms": 0,
            "last_tool_call_completed_ms": 0,
            "uptime_ms": 1000,
        });
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("old payload must still deserialize");
        assert_eq!(back.withhold_reason, None);
        assert_eq!(back.withhold_since_ms, None);
        assert!(!back.withhold_capped);
        assert_eq!(back.preview_ws_tunnels_open, 0);
        // An absent reason is indistinguishable from "nothing is withheld" —
        // what the field-coverage ratio exists to measure.
    }

    /// A legacy payload without the new fields deserializes with defaults.
    #[test]
    fn tool_server_status_payload_legacy_without_pr9_fields_defaults() {
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(legacy_status_json()).expect("legacy payload must deserialize");
        assert_eq!(back.status, super::ToolServerLifecycleStatus::Ready);
        assert_eq!(back.upload_queue_pending, 0);
        assert_eq!(back.upload_queue_pending_bytes, 0);
        assert_eq!(back.upload_queue_inflight, 0);
        assert!(!back.upload_queue_circuit_breaker_tripped);
        assert_eq!(back.artifact_producers_inflight, 0);
        assert_eq!(back.drain_started_ms, None);
        assert!(!back.turn_active);
    }

    #[test]
    fn tool_server_status_payload_idle_ignores_background_round_trips() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Ready,
            idle_ignores_background: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["idle_ignores_background"], true);
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert!(back.idle_ignores_background);

        let default_json =
            serde_json::to_value(super::ToolServerStatusPayload::default()).expect("serialize");
        assert_eq!(default_json["idle_ignores_background"], false);

        let back: super::ToolServerStatusPayload =
            serde_json::from_value(legacy_status_json()).expect("deserialize");
        assert!(!back.idle_ignores_background);
    }

    #[test]
    fn tool_server_status_payload_drain_started_skipped_when_none() {
        let payload = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Ready,
            ..Default::default()
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert!(
            json.get("drain_started_ms").is_none(),
            "None drain_started_ms should be skipped on the wire"
        );
    }

    #[test]
    fn tool_server_status_payload_connection_id_round_trips_and_skips_none() {
        let with_conn = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Disconnected,
            connection_id: Some("ts-dead".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&with_conn).expect("serialize");
        assert_eq!(json["connection_id"], "ts-dead");
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.connection_id.as_deref(), Some("ts-dead"));

        let without = super::ToolServerStatusPayload {
            status: super::ToolServerLifecycleStatus::Disconnected,
            ..Default::default()
        };
        let json = serde_json::to_value(&without).expect("serialize");
        assert!(json.get("connection_id").is_none());
        // Round-trip the None case through the payload's own serialized form.
        let back: super::ToolServerStatusPayload =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.connection_id, None);

        // Separately, a legacy payload that omits the `connection_id` field
        // entirely still deserializes (forward/backward wire compat).
        let legacy: super::ToolServerStatusPayload =
            serde_json::from_value(legacy_status_json()).expect("legacy payload must deserialize");
        assert_eq!(legacy.connection_id, None);
    }

    #[test]
    fn tool_server_evict_params_round_trips() {
        let params = super::ToolServerEvictParams {
            session_id: sid(),
            reason: "idle_timeout".into(),
            grace_period_ms: 30000,
        };
        let json = serde_json::to_value(&params).expect("serialize");
        let back: super::ToolServerEvictParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    #[test]
    fn tool_server_get_status_params_round_trips() {
        let params = super::ToolServerGetStatusParams { session_id: sid() };
        let json = serde_json::to_value(&params).expect("serialize");
        let back: super::ToolServerGetStatusParams =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, params);
    }

    #[test]
    fn tool_server_get_status_result_round_trips() {
        let result = super::ToolServerGetStatusResult {
            tool_servers: vec![super::ToolServerConnectionStatus {
                connection_id: "conn-1".into(),
                status: super::ToolServerStatusPayload {
                    status: super::ToolServerLifecycleStatus::Ready,
                    session_id: None,
                    active_tool_calls: 0,
                    active_tool_names: vec![],
                    background_tasks: 0,
                    background_task_ids: vec![],
                    pending_tool_calls: 0,
                    last_tool_call_started_ms: 0,
                    last_tool_call_completed_ms: 0,
                    uptime_ms: 120000,
                    idle_since_ms: Some(1721234560000),
                    ..Default::default()
                },
            }],
        };
        let json = serde_json::to_value(&result).expect("serialize");
        let back: super::ToolServerGetStatusResult =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, result);
    }

    #[test]
    fn tool_server_disconnect_reason_serde() {
        use super::ToolServerDisconnectReason::*;
        for (variant, expected_str) in [
            (NormalClose, "normal_close"),
            (IdleTimeout, "idle_timeout"),
            (ForceEvicted, "force_evicted"),
            (ConnectionLost, "connection_lost"),
        ] {
            let json = serde_json::to_value(variant).expect("serialize");
            assert_eq!(json.as_str(), Some(expected_str));
            let back: super::ToolServerDisconnectReason =
                serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn notification_custom_round_trips_through_serde() {
        let frame = ToolNotificationFrame::custom(tid(), "test.kind", json!({"x": 42}));
        let json = serde_json::to_value(&frame).expect("serialize");
        let back: ToolNotificationFrame = serde_json::from_value(json).expect("deserialize");
        assert_eq!(frame, back);
    }

    // ── hook_id backward-compat ─────────────────────────────────────

    #[test]
    fn hook_frame_missing_hook_id_deserializes_as_none() {
        let v = json!({
            "session_id": "test-session",
            "event": { "type": "Pause" },
        });
        let frame: HookFrame = serde_json::from_value(v).expect("deserialize");
        assert!(frame.hook_id.is_none());
    }

    #[test]
    fn hook_frame_with_hook_id_round_trips() {
        let mut hook = HookFrame::pause(sid());
        hook.hook_id = Some("abc".into());
        let v = serde_json::to_value(&hook).expect("serialize");
        assert_eq!(v["hook_id"], "abc");
        let back: HookFrame = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back.hook_id, Some("abc".into()));
    }

    #[test]
    fn hook_frame_none_hook_id_skipped_in_json() {
        let hook = HookFrame::pause(sid());
        assert!(hook.hook_id.is_none());
        let v = serde_json::to_value(&hook).expect("serialize");
        assert!(
            v.as_object().unwrap().get("hook_id").is_none(),
            "None hook_id must be absent from JSON"
        );
    }

    #[test]
    fn hook_frame_empty_string_hook_id_round_trips() {
        let mut hook = HookFrame::pause(sid());
        hook.hook_id = Some(String::new());
        let v = serde_json::to_value(&hook).expect("serialize");
        assert_eq!(v["hook_id"], "");
        let back: HookFrame = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back.hook_id, Some(String::new()));
    }

    // ── Image capability tokens ─────────────────────────────────────
    //
    // The canonical accept/reject lists for every hop: the workspace-side
    // reader and the chat-side re-validation both call this gate, so a rule
    // change has to break here first.

    #[test]
    fn image_capability_tokens_accepted() {
        for name in [
            IMAGE_CAPABILITIES_V1,
            "echo-agent-files.occ",
            "playwright.chromium-headless-shell",
            "app-template.v2",
            "a.b.c",
            "node.22",
            &format!("a.{}", "b".repeat(MAX_IMAGE_CAPABILITY_LEN - 2)),
        ] {
            assert!(is_image_capability_token(name), "expected valid: {name}");
        }
    }

    #[test]
    fn image_capability_tokens_rejected() {
        for name in [
            "",
            "README",
            "..",
            ".hidden",
            "trailing.",
            "Grove.Daemon",
            "echo_agent_files.occ",
            "-lead.ing",
            "trail-.ing",
            "seg..empty",
            "space d.ot",
            "unicode.é",
            "single",
            "../etc/passwd",
            &format!("a.{}", "b".repeat(MAX_IMAGE_CAPABILITY_LEN)),
        ] {
            assert!(!is_image_capability_token(name), "expected invalid: {name}");
        }
    }
}
