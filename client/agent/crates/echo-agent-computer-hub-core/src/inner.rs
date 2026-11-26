//! `InnerDispatchForResolver` — an object-safe `ToolDispatch` that routes
//! through a `Weak<CompoundResolver>` bound to a single session.
//!
//! Tools that need to call other tools (the inner-dispatch pattern) ask
//! the runtime for an `Arc<dyn ToolDispatch>`. This adapter answers that
//! question with a resolver-backed implementation. Holding the resolver
//! by [`Weak`] lets the router own the resolver while inner-dispatch
//! handles created from the same resolver release naturally when the
//! router is torn down.

use std::sync::Weak;

use async_trait::async_trait;
use serde_json::Value;

use echo_agent_tool_protocol::{SessionId, ToolId};
use echo_agent_tool_runtime::{
    ToolCallContext, ToolDispatch, ToolError, ToolStream, TypedToolOutput, terminal_only,
};

use crate::resolver::CompoundResolver;

/// Resolver-backed `ToolDispatch` implementation.
///
/// The resolver is held by [`Weak`] so the inner-dispatch handle never
/// keeps the router alive past its natural lifetime — when the owning
/// router drops the resolver, in-flight inner calls fail cleanly with
/// [`ToolError::Custom`] keyed `computer_hub_dropped`.
///
/// Bound to a single [`SessionId`] at construction (rather than reading a
/// session from [`ToolCallContext`]) so the inner-dispatch path mirrors
/// the per-session lifetime of the outer router.
#[derive(Debug)]
pub struct InnerDispatchForResolver {
    resolver: Weak<CompoundResolver>,
    session_id: SessionId,
}

impl InnerDispatchForResolver {
    /// Build an inner-dispatch handle bound to `session_id`, resolving
    /// through `resolver`.
    pub fn new(resolver: Weak<CompoundResolver>, session_id: SessionId) -> Self {
        Self {
            resolver,
            session_id,
        }
    }

    /// Borrow the bound session identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

#[async_trait]
impl ToolDispatch for InnerDispatchForResolver {
    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        let Some(resolver) = self.resolver.upgrade() else {
            return terminal_only(Err(ToolError::custom(
                "computer_hub_dropped",
                "computer hub dropped before inner call could execute",
            )));
        };
        resolver
            .resolve_and_dispatch(&self.session_id, tool_id, args, ctx)
            .await
    }
}
