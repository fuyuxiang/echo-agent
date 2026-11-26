//! In-process transport that resolves through a [`CompoundResolver`].
//!
//! `LocalTransport` is bound to a single `(user_id, session_id)` at
//! construction. Authorisation returns a principal pre-populated with the
//! bound session and the `tool.invoke` scope; per-call dispatch resolves
//! against the bound session's view of the resolver.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use echo_agent_tool_protocol::{SessionId, ToolId, UserId};
use echo_agent_tool_runtime::{ToolCallContext, ToolError, ToolStream, TypedToolOutput};

use crate::resolver::CompoundResolver;
use crate::transport::{Principal, Transport, TransportKind};

/// The scope `LocalTransport::authorize` grants to its principal.
///
/// Hoisted so adapters that authorise principals through other paths
/// can match the local convention without restating the literal.
pub const LOCAL_INVOKE_SCOPE: &str = "tool.invoke";

/// Transport that dispatches against an in-process resolver.
#[derive(Debug)]
pub struct LocalTransport {
    resolver: Arc<CompoundResolver>,
    user_id: UserId,
    session_id: SessionId,
}

impl LocalTransport {
    /// Build a transport bound to `(user_id, session_id)` and resolving
    /// through `resolver`.
    pub fn new(resolver: Arc<CompoundResolver>, user_id: UserId, session_id: SessionId) -> Self {
        Self {
            resolver,
            user_id,
            session_id,
        }
    }

    /// Bound user identity for this transport.
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    /// Bound session for this transport.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

#[async_trait]
impl Transport for LocalTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Local
    }

    async fn authorize(&self) -> Result<Principal, ToolError> {
        Ok(Principal::new(self.user_id.clone())
            .with_session(self.session_id.clone())
            .with_scope(LOCAL_INVOKE_SCOPE))
    }

    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        self.resolver
            .resolve_and_dispatch(&self.session_id, tool_id, args, ctx)
            .await
    }
}
