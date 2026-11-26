//! `CompoundResolver` plus the `ResolvedTool` and `ToolHandle`
//! types it returns.
//!
//! `Tool` carries associated `Args` / `Output` types and is therefore not
//! object-safe. [`ToolHandle`] is the dyn-compatible projection used
//! by every router build: typed tools are wrapped via
//! [`ErasedTool::new`]; remote registrations expose
//! [`crate::RemoteToolProxy`] which implements [`ToolHandle`]
//! directly without an intermediate typed `Tool` impl.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use echo_agent_tool_protocol::{SessionId, ToolCapabilities, ToolId, ToolRegistration};
use echo_agent_tool_runtime::{
    ListToolsContext, Tool, ToolCallContext, ToolError, ToolOutput, ToolStream, ToolStreamItem,
    TypedToolOutput, terminal_only,
};
use echo_agent_tool_types::ToolDescription;

use crate::registry::ToolRegistry;

/// Active resolution returned by [`CompoundResolver::resolve`].
///
/// Variants share the same `tool` handle and `registration` shape; the
/// discriminant only tells callers whether the executing handle dispatches
/// in-process or forwards over a connection. Differentiating the variants
/// is useful for metrics, log tags, and the local-shadows-remote rule
/// applied when both planes register the same `tool_id`.
#[derive(Debug, Clone)]
pub enum ResolvedTool {
    /// In-process tool resolved from the local registry.
    Local {
        /// Object-safe handle to the tool's `execute` entry point.
        tool: Arc<dyn ToolHandle>,
        /// Wire-shape registration record. Carries `tool_id`, the
        /// schema-bearing description, capabilities, and ownership data.
        registration: ToolRegistration,
    },
    /// Remote registration resolved through a connection-backed proxy.
    Remote {
        /// Object-safe handle whose `execute` forwards over the
        /// owning connection.
        proxy: Arc<dyn ToolHandle>,
        /// Wire-shape registration record (same shape as the local
        /// variant — both store the active registration so callers do not
        /// have to round-trip the registry for description / capabilities).
        registration: ToolRegistration,
    },
}

impl ResolvedTool {
    /// Borrow the registration record regardless of variant.
    pub fn registration(&self) -> &ToolRegistration {
        match self {
            Self::Local { registration, .. } | Self::Remote { registration, .. } => registration,
        }
    }

    /// Borrow the executing handle regardless of variant.
    pub fn handle(&self) -> &Arc<dyn ToolHandle> {
        match self {
            Self::Local { tool, .. } => tool,
            Self::Remote { proxy, .. } => proxy,
        }
    }
}

/// Object-safe projection of a registered tool.
///
/// The router only needs identity, description, capabilities, and a
/// JSON-typed `execute` entry point — exactly what this trait exposes.
/// Adapters that wrap a typed `Tool` impl get [`ErasedTool`] for free;
/// non-`Tool` handles (notably remote proxies) implement this trait
/// directly.
#[async_trait]
pub trait ToolHandle: Send + Sync + std::fmt::Debug {
    /// Stable identity used by the router to route calls.
    fn id(&self) -> ToolId;

    /// Model-facing description of the tool's argument schema.
    ///
    /// Receives the per-turn [`ListToolsContext`] so handles backed by a
    /// typed [`Tool`] can produce context-aware descriptions at listing
    /// time. Callers outside a listing turn pass
    /// [`ListToolsContext::default`].
    fn description(&self, ctx: &ListToolsContext) -> ToolDescription;

    /// Per-tool capability flags.
    fn capabilities(&self) -> ToolCapabilities;

    /// Per-turn listing predicate.
    fn should_list(&self, _ctx: &ListToolsContext) -> bool {
        true
    }

    /// Streaming execution entry point.
    ///
    /// Implementations encode the tool's typed `Output` to
    /// [`serde_json::Value`] and surface argument-decoding failures as
    /// [`ToolError::InvalidArguments`] within the terminal item.
    async fn execute(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput>;
}

/// Type-erasing wrapper for any [`Tool`] implementation.
///
/// Decodes `args` into `T::Args`, drives `T::execute`, and re-encodes each
/// `T::Output` (terminal and progress items pass through unchanged
/// otherwise). The wrapper holds the inner tool by `Arc` so the same
/// underlying instance can back multiple registrations cheaply.
pub struct ErasedTool<T> {
    inner: Arc<T>,
}

impl<T> ErasedTool<T> {
    /// Wrap an `Arc<T>` for use as an [`ToolHandle`].
    pub fn from_arc(inner: Arc<T>) -> Self {
        Self { inner }
    }

    /// Wrap an owned tool, taking the `Arc` allocation internally.
    pub fn new(inner: T) -> Self {
        Self::from_arc(Arc::new(inner))
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ErasedTool<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedTool")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T> Clone for ErasedTool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[async_trait]
impl<T> ToolHandle for ErasedTool<T>
where
    T: Tool + std::fmt::Debug + 'static,
    T::Output: ToolOutput,
{
    fn id(&self) -> ToolId {
        self.inner.id()
    }

    fn description(&self, ctx: &ListToolsContext) -> ToolDescription {
        self.inner.description(ctx)
    }

    fn capabilities(&self) -> ToolCapabilities {
        self.inner.capabilities()
    }

    fn should_list(&self, ctx: &ListToolsContext) -> bool {
        self.inner.should_list(ctx)
    }

    async fn execute(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput> {
        let typed_args: T::Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return terminal_only(Err(ToolError::invalid_arguments(e.to_string())));
            }
        };
        let tool_id = self.inner.id();
        let stream = self.inner.execute(ctx, typed_args).await;
        let mapped = stream.map(move |item| match item {
            ToolStreamItem::Progress(p) => ToolStreamItem::Progress(p),
            ToolStreamItem::Terminal(Ok(out)) => match serde_json::to_value(&out) {
                Ok(value) => {
                    let custom = out.model_output();
                    let model_output = if custom.is_empty() {
                        echo_agent_tool_runtime::extract_content_blocks(&value)
                    } else {
                        custom
                    };
                    let chat_completion_output = out.chat_completion_output();
                    ToolStreamItem::Terminal(Ok(TypedToolOutput {
                        tool_id: tool_id.clone(),
                        value,
                        model_output,
                        chat_completion_output,
                    }))
                }
                Err(e) => ToolStreamItem::Terminal(Err(ToolError::custom(
                    "output_encoding",
                    e.to_string(),
                ))),
            },
            ToolStreamItem::Terminal(Err(err)) => ToolStreamItem::Terminal(Err(err)),
        });
        Box::pin(mapped)
    }
}

/// Compose a local-first lookup over one (`local_only`) or two
/// (`compound`) registries.
///
/// The lookup contract: `find_tool` is called on the local registry first;
/// only if it returns `None` is the remote registry consulted. Any local
/// registration shadows a same-id remote registration. Cross-session
/// lookups return `None` — the caller may surface this as a
/// [`ToolError::NotFound`] to keep ownership invisible to the requester.
#[derive(Debug)]
pub struct CompoundResolver {
    local: Arc<dyn ToolRegistry>,
    remote: Option<Arc<dyn ToolRegistry>>,
}

impl CompoundResolver {
    /// Compose a resolver that consults a single local registry.
    pub fn local_only(local: Arc<dyn ToolRegistry>) -> Self {
        Self {
            local,
            remote: None,
        }
    }

    /// Compose a resolver with both planes; `local` is consulted first.
    pub fn compound(local: Arc<dyn ToolRegistry>, remote: Arc<dyn ToolRegistry>) -> Self {
        Self {
            local,
            remote: Some(remote),
        }
    }

    /// Borrow the local plane.
    pub fn local(&self) -> &Arc<dyn ToolRegistry> {
        &self.local
    }

    /// Borrow the optional remote plane.
    pub fn remote(&self) -> Option<&Arc<dyn ToolRegistry>> {
        self.remote.as_ref()
    }

    /// Resolve `(session, tool_id)` honouring the local-first rule.
    pub fn resolve(&self, session: &SessionId, tool_id: &ToolId) -> Option<ResolvedTool> {
        if let Some(hit) = self.local.find_tool(session, tool_id) {
            return Some(hit);
        }
        self.remote
            .as_ref()
            .and_then(|r| r.find_tool(session, tool_id))
    }

    /// Resolve `(session, tool_id)` and dispatch through the active
    /// handle, returning the tool's stream verbatim. Misses produce a
    /// single-item terminal stream carrying [`ToolError::NotFound`].
    ///
    /// Centralises the resolve-then-dispatch sequence so both the
    /// transport-side `LocalTransport::call` and the inner-dispatch path
    /// share one implementation: a future change to the miss-shape (or
    /// to the dispatch contract) lands once.
    pub async fn resolve_and_dispatch(
        &self,
        session: &SessionId,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        match self.resolve(session, &tool_id) {
            Some(resolved) => resolved.handle().execute(ctx, args).await,
            None => terminal_only(Err(ToolError::not_found(
                tool_id.clone(),
                format!("tool not found: {tool_id}"),
            ))),
        }
    }
}
