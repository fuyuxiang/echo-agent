//! Registration payloads, descriptions, transports, and outcomes.

use serde::{Deserialize, Serialize};

use crate::{
    HookKind, IdError, NotificationSchemas, ServerId, SessionId, ToolCapabilities, ToolId, UserId,
};

/// Whether a registered tool runs in-process or behind a remote connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Local,
    Remote,
}

/// A single tool's wire description plus optional schema and capability
/// metadata. The `tool_id` is **not** stored explicitly — it is derived
/// from `description.{namespace, name}` via [`Self::derive_tool_id`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptionWithSchema {
    pub description: echo_agent_tool_types::ToolDescription,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ToolCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_schemas: Option<NotificationSchemas>,
}

impl ToolDescriptionWithSchema {
    /// Derive the canonical `ToolId`.
    ///
    /// Namespaced descriptions render as `"{namespace}:{name}"`; otherwise
    /// the bare `name`. The result is run through [`ToolId::new`], so an
    /// invalid name or namespace surfaces as an [`IdError`].
    pub fn derive_tool_id(&self) -> Result<ToolId, IdError> {
        match &self.description.namespace {
            Some(ns) => ToolId::new(format!("{ns}:{}", self.description.name)),
            None => ToolId::new(self.description.name.as_str()),
        }
    }
}

/// Single-tool registration. Wire-level sugar for a one-tool
/// `register_server`.
///
/// `sessions` carries the per-tool session set with three-state
/// semantics, modelled after `if_match_generation: Option<u64>`:
///
/// - `None` (field omitted on the wire) — "no change". For a
///   first-time registration the computer hub treats this as the empty
///   set; for a re-registration the existing session bindings are
///   preserved untouched. Use this for heartbeat-style re-register
///   flows that re-send the description without revisiting the
///   session set.
/// - `Some(vec![])` (explicit empty array) — "unbind every session".
///   The tool stays registered against the connection but becomes
///   unreachable from any session until [`crate::Method::BindToolSession`]
///   adds a new binding.
/// - `Some(vec![s1, ...])` — replace the per-tool session set with
///   exactly the listed ids. Each id MUST already be in the
///   connection's bound-session set (validated by the router);
///   a missing id rejects the entire registration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRegistration {
    /// MUST equal `description.derive_tool_id()`. Carried explicitly so
    /// receivers can route without re-deriving. The IC service router
    /// enforces this at register-tool time and rejects mismatches with
    /// `InvalidRequest`.
    pub tool_id: ToolId,
    /// Per-tool session set. See struct doc-comment for the
    /// `None` / `Some(vec![])` / `Some(vec![...])` semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionId>>,
    pub user_id: UserId,
    /// `None` → computer hub synthesises `auto:tool:{tool_id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<ServerId>,
    pub description: echo_agent_tool_types::ToolDescription,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ToolCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_schemas: Option<NotificationSchemas>,
    pub transport_kind: TransportKind,
    /// Optimistic-concurrency precondition. `None` → last-writer-wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match_generation: Option<u64>,
    /// Opaque metadata supplied by the tool server at registration time.
    /// Propagated to `ServerInfo.metadata` in `servers.list` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ToolRegistration {
    /// Derive the canonical `ToolId` from `description.{namespace, name}`.
    /// The `tool_id` payload field MUST equal this value; the IC service
    /// router enforces the invariant at register-tool time.
    pub fn derive_tool_id(&self) -> Result<ToolId, IdError> {
        match &self.description.namespace {
            Some(ns) => ToolId::new(format!("{ns}:{}", self.description.name)),
            None => ToolId::new(self.description.name.as_str()),
        }
    }
}

/// Multi-tool registration. The whole batch shares one `server_id` and one
/// `sessions` value; per-tool outcomes are reported individually via
/// [`RegistrationOutcome`].
///
/// `sessions` follows the same three-state semantics as
/// [`ToolRegistration::sessions`]: `None` means "no change" (preserves
/// existing per-tool session bindings on a re-register), `Some(vec![])`
/// means "unbind every session for every tool in this batch", and
/// `Some(vec![...])` means "replace each tool's session set with
/// exactly these ids".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServerRegistration {
    pub server_id: ServerId,
    /// Per-batch session set. See struct doc-comment for `None` /
    /// `Some(vec![])` / `Some(vec![...])` semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionId>>,
    pub user_id: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub tools: Vec<ToolDescriptionWithSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match_generation: Option<u64>,
    /// Opaque metadata supplied by the tool server at registration time.
    /// Applied to every tool in the batch and propagated to
    /// `ServerInfo.metadata` in `servers.list` responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Per-tool result from a `register_tool` or `register_server` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RegistrationOutcome {
    Registered {
        tool_id: ToolId,
        generation: u64,
    },
    Updated {
        tool_id: ToolId,
        generation: u64,
    },
    Shadowed {
        tool_id: ToolId,
        reason: String,
    },
    Rejected {
        tool_id: ToolId,
        code: String,
        message: String,
    },
}
