//! Handshake messages exchanged immediately after the WebSocket upgrade.

use serde::{Deserialize, Serialize};

use crate::{ConnectionId, ConnectionKind, ServerId, UserId};

/// Wire-protocol version both ends speak. Bumped when an incompatible
/// schema change lands; minor additions go through capability
/// negotiation rather than a version bump.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// First frame sent by the client after the WebSocket upgrade succeeds.
///
/// No session ids are carried at handshake time. The connection starts with
/// an empty bound-session set and binds sessions dynamically over its
/// lifetime via `register_session` / `unregister_session` JSON-RPC calls.
///
/// Tool-server connections carry `server_id` so the hub can
/// identify the server without a separate `register_server` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloMsg {
    pub protocol_version: String,
    pub kind: ConnectionKind,
    /// Stable server identity. Only set for
    /// [`ConnectionKind::ToolServer`] connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<ServerId>,
    /// One-line server description for `servers.list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Opaque metadata surfaced in `ServerInfo.metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Computer hub's reply to [`HelloMsg`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloAckMsg {
    pub connection_id: ConnectionId,
    /// Hub-derived user identity. The hub resolves this from the
    /// upgrade credential (JWT `sub`, local-dev hash, etc.) so the
    /// client never needs to announce it.
    pub user_id: UserId,
    pub computer_hub_version: String,
    pub supported_protocol_versions: Vec<String>,
    /// Optional JSON-RPC methods this hub supports beyond the base
    /// protocol (wire method strings, e.g. `"session_attach_server"`).
    /// Absent on hubs predating the field; additive, so clients gate
    /// per-call fallbacks on membership instead of probing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}
