//! Object-safe `Transport` trait plus the `Principal` value carried across
//! authorize/call boundaries.
//!
//! [`TransportKind`] is re-exported from [`echo_agent_tool_protocol`] so the wire
//! and dispatch layers share one canonical enum and there is no duplicate
//! `Local` / `Remote` definition to keep in sync.

use async_trait::async_trait;
use serde_json::Value;

use echo_agent_tool_protocol::{SessionId, ToolId, UserId};
use echo_agent_tool_runtime::{ToolCallContext, ToolError, ToolStream, TypedToolOutput};

pub use echo_agent_tool_protocol::TransportKind;

/// Authenticated identity bound to a transport at handshake time.
///
/// The transport authorises **once** at connect; subsequent dispatch calls
/// carry no extra credentials. `session_ids` is plural because a JWT may
/// authorise more than one session (multi-tenant tooling sessions sharing
/// a single user identity); the router narrows by [`SessionId`] at the
/// per-call boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Authenticated user identity.
    pub user_id: UserId,

    /// Sessions this principal is authorised to act on. Empty when the
    /// transport authorises a user but has not yet bound a session
    /// (e.g. a fresh harness connection that has not opened a session).
    pub session_ids: Vec<SessionId>,

    /// OAuth-style scopes granted to this principal, e.g. `"tool.invoke"`.
    pub scopes: Vec<String>,

    /// Token audiences claimed by the credential, e.g. the router's
    /// expected `aud` values. Used by callers that need defence-in-depth
    /// audience checks beyond what the transport already validated.
    pub audiences: Vec<String>,
}

impl Principal {
    /// Build a principal for `user_id` with no sessions, scopes, or
    /// audiences. Use the `with_*` builders to populate the rest.
    pub fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            session_ids: Vec::new(),
            scopes: Vec::new(),
            audiences: Vec::new(),
        }
    }

    /// Append `session_id` to the authorised set.
    pub fn with_session(mut self, session_id: SessionId) -> Self {
        self.session_ids.push(session_id);
        self
    }

    /// Append `scope` to the granted scopes.
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    /// Append `aud` to the token's audience list.
    pub fn with_audience(mut self, aud: impl Into<String>) -> Self {
        self.audiences.push(aud.into());
        self
    }

    /// Whether `scope` is present in the granted scopes.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// Whether `session_id` is in the principal's authorised session set.
    pub fn authorizes_session(&self, session_id: &SessionId) -> bool {
        self.session_ids.iter().any(|s| s == session_id)
    }
}

/// Object-safe transport for dispatching tool calls.
///
/// Implementations come in two flavours: [`TransportKind::Local`] resolves
/// against an in-process registry, while [`TransportKind::Remote`] forwards
/// a `tool_call_request` over a [`crate::ConnectionClient`].
#[async_trait]
pub trait Transport: Send + Sync + std::fmt::Debug {
    /// Whether the underlying transport is local (in-process) or remote
    /// (forwarded over a connection).
    fn kind(&self) -> TransportKind;

    /// One-time authorisation handshake.
    ///
    /// Local transports return a principal derived from the bound OS user
    /// (or whatever ambient identity the host process provides). Remote
    /// transports return the principal extracted from a validated
    /// credential. Subsequent [`Self::call`] invocations reuse this
    /// principal — the router never re-authorises per call.
    async fn authorize(&self) -> Result<Principal, ToolError>;

    /// Dispatch a tool call.
    ///
    /// The returned [`ToolStream`] follows the runtime invariant: zero or
    /// more `Progress` items followed by exactly one `Terminal`. A
    /// not-found result is reported as a single-item terminal stream
    /// carrying [`ToolError::NotFound`]; transport-level disconnects
    /// surface as [`ToolError::NetworkError`].
    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput>;
}
