//! Wire-friendly error type carried inside the JSON-RPC `error.data` field.

use serde::{Deserialize, Serialize};

use crate::{RequestId, ToolId};

/// Stable wire representation of a tool-call failure.
///
/// Receivers SHOULD switch on the `code` discriminator (e.g.
/// `tool_not_found`) rather than the numeric JSON-RPC `error.code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ToolErrorWire {
    #[error("tool not found: {tool_id}")]
    ToolNotFound { tool_id: ToolId },

    #[error("session mismatch")]
    SessionMismatch,

    #[serde(rename = "forbidden")]
    #[error("permission denied: {reason}")]
    PermissionDenied { reason: String },

    #[serde(rename = "connection_lost")]
    #[error("transport closed for {tool_id}")]
    TransportClosed { tool_id: ToolId },

    #[error("timeout after {elapsed_ms}ms for {tool_id}")]
    Timeout { tool_id: ToolId, elapsed_ms: u64 },

    #[error("cancelled")]
    Cancelled { tool_id: ToolId },

    #[serde(rename = "invalid_params")]
    #[error("invalid arguments: {message}")]
    InvalidArguments {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },

    #[error("execution error in {tool_id}: {message}")]
    Execution { tool_id: ToolId, message: String },

    #[error("unsupported protocol version")]
    UnsupportedProtocolVersion { supported: Vec<String> },

    #[serde(rename = "frame_too_large")]
    #[error("payload too large: {bytes} bytes (limit {limit})")]
    PayloadTooLarge { bytes: u64, limit: u64 },

    #[error("behavior_version unsupported")]
    BehaviorVersionUnsupported { tool_id: ToolId, requested: String },

    /// Render-card budget exceeded for the current session. `card_id`
    /// carries the offending render-card identifier when known; `reason`
    /// is a free-form human-readable explanation.
    #[error("render limited for {tool_id}: {reason}")]
    RenderLimited {
        tool_id: ToolId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_id: Option<String>,
        reason: String,
    },

    /// Terminal-tool subprocess sub-call failed. Distinct from
    /// `Execution` because terminal sub-call failures have a known,
    /// retry-eligible shape.
    #[error("terminal subprocess error in {tool_id}: {message}")]
    TerminalError { tool_id: ToolId, message: String },

    #[serde(rename = "internal_error")]
    #[error("internal error{}", .detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    Internal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
        /// Bounded, human-readable cause of the internal error. Optional for
        /// wire compatibility with older peers; producers SHOULD populate it
        /// (truncated at the producer) so receivers can distinguish failure
        /// modes without correlating server logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// Free-form forward-compat error. The outer `code` discriminator is
    /// always the literal `"custom"`; the producer-supplied subcode lives
    /// in `subcode` (the field can't be named `code` because it would
    /// collide with the serde discriminator).
    #[error("custom: {subcode} — {message}")]
    Custom {
        subcode: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
}
