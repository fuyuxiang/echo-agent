//! Harness-side dispatch surface.
//!
//! A [`ToolHarness`] is the SDK-side counterpart to a server-side
//! `Harness` connection. Build it via [`ToolHarnessBuilder`], seed
//! the in-process [`LocalRegistry`] with `Tool` implementations that
//! should resolve without a wire round-trip, and call
//! [`ToolHarness::call`] to dispatch a tool.
//!
//! Local-first dispatch: every call queries the bound
//! [`LocalRegistry`] first; on a hit the tool's `execute` runs
//! in-process and the returned [`ToolStream`] is forwarded verbatim.
//! Misses fall through to a remote `tool.call` JSON-RPC request over
//! the shared [`HubConnection`]; the demux routes the matching
//! response and any intermediate `tool_call_progress` notifications
//! back into the call's stream.
//!
//! Connection lifecycle mirrors [`crate::ToolServer`]: the harness
//! attaches to a pooled connection under a `(url, principal)` key,
//! refcount-binds its session, and runs cooperative shutdown
//! through a `ConnectionBorrow` (crate-internal) that
//! both ends share.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use dashmap::DashMap;
use echo_agent_computer_hub_core::{
    ErasedTool, ToolHandle, decode_call_result, error_from_envelope, progress_from_frame,
    tool_error_from_wire,
};
use echo_agent_tool_protocol::notification_wire::{WireCustomNotification, WireToolNotification};
use echo_agent_tool_protocol::session_event::{SessionEvent, ToolCallOutcome};
use echo_agent_tool_protocol::{
    ConnectionKind, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    JsonRpcVersion, Method, RequestId, ResponseOutcome, SessionId, ToolCallId, ToolCallParams,
    ToolCallProgressFrame, ToolId, ToolNotificationFrame, ToolServerLifecycleStatus,
    WorkspaceGonePhase, WorkspaceGoneReason, workspace_unavailable_wire,
};
use echo_agent_tool_runtime::{
    BehaviorVersion, Cwd, ListToolsContext, Tool, ToolCallContext, ToolError, ToolStream,
    ToolStreamItem, TypedToolOutput, terminal_only,
};
use echo_agent_tool_types::ToolDescription;
use futures::FutureExt;
use futures::Stream;
use futures::future::{BoxFuture, Shared};
use indexmap::IndexMap;
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use crate::auth::{AuthCredential, AuthProvider};
use crate::connection::{HubConnection, ReconnectCallback, ReconnectEvent};
use crate::connection_borrow::ConnectionBorrow;
use crate::error::ClientError;
use crate::pool::HubConnectionPool;

/// Host-supplied source of the current W3C `traceparent`.
pub type TraceContextProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Host-registered sink for inbound reverse-direction hook requests
/// (server → harness); invoked by the inbox loop with the decoded
/// [`HookFrame`](echo_agent_tool_protocol::HookFrame), answered via
/// [`ToolHarness::send_hook_reply`].
type HookRequestHandler = Arc<dyn Fn(echo_agent_tool_protocol::HookFrame) + Send + Sync>;

/// Well-known [`HookEvent::Custom`](echo_agent_tool_protocol::HookEvent::Custom) kind
/// for a server → harness permission request. Sibling of
/// [`echo_agent_tool_protocol::turn_hook::TURN_HOOK_KIND`].
pub const PERMISSION_REQUEST_KIND: &str = "permission_request";

/// Buffer size for the per-call progress channel. Picked to absorb a
/// brief consumer pause without blocking the connection actor's
/// inbound dispatch loop. A slow stream consumer surfaces as
/// `RouteOutcome::ProgressFull` and the dropped frame is logged.
const PROGRESS_BUFFER: usize = 64;

/// Opt-in per-call flag (a [`ToolCallContext`] extension): when set to
/// `true`, the remote call's [`ToolStream`] emits one best-effort
/// call-scoped cancel hook on `Drop` so the workspace hard-cancels the
/// in-flight call. Absent or `false` (the default) → `Drop` emits
/// nothing. No effect on local-dispatch calls.
#[derive(Clone, Copy, Debug)]
pub struct CancelOnDrop(pub bool);

pub type ModelOutputExtractor =
    Arc<dyn Fn(&Value) -> Option<Vec<echo_agent_tool_runtime::ContentBlock>> + Send + Sync>;

pub fn extractor_for<T>() -> ModelOutputExtractor
where
    T: echo_agent_tool_runtime::ToolOutput + serde::de::DeserializeOwned + 'static,
{
    Arc::new(|value: &Value| {
        serde_json::from_value::<T>(value.clone())
            .ok()
            .map(|output| output.model_output().to_vec())
    })
}

/// In-process registry of tool handles owned by a [`ToolHarness`].
///
/// Tools registered here resolve in-process — `ToolHarness::call`
/// short-circuits the wire dispatch and invokes the handle directly.
/// Mutations are concurrency-safe (`RwLock` on `entries`, `DashMap`
/// on `extractors`), so callers MAY hot-add or hot-remove tools
/// while the harness is in use.
///
/// `entries` uses `RwLock<IndexMap>` to preserve insertion order so
/// that `list_tools` returns descriptions in the same order tools
/// were registered (matching the config-defined order).
///
/// Optionally stores a per-tool [`ModelOutputExtractor`] for client-side
/// model output extraction. Use [`register_with_model_output`](Self::register_with_model_output)
/// to capture the extractor at registration time, or [`register_extractor`](Self::register_extractor)
/// to add one separately.
#[derive(Default)]
struct LocalRegistryInner {
    entries: RwLock<IndexMap<ToolId, Arc<dyn ToolHandle>>>,
    extractors: DashMap<ToolId, ModelOutputExtractor>,
}

#[derive(Clone, Default)]
pub struct LocalRegistry {
    inner: Arc<LocalRegistryInner>,
}

impl std::fmt::Debug for LocalRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRegistry")
            .field("entries", &self.inner.entries.read().len())
            .field("extractors", &self.inner.extractors.len())
            .finish()
    }
}

impl LocalRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed [`Tool`] implementation by value. Subsequent
    /// registrations of the same id replace the previous handle and
    /// return the displaced handle for inspection / drop ordering.
    pub fn register<T>(&self, tool: T) -> Option<Arc<dyn ToolHandle>>
    where
        T: Tool + std::fmt::Debug + 'static,
    {
        self.register_arc(Arc::new(tool))
    }

    /// Register a typed [`Tool`] implementation already wrapped in `Arc`.
    pub fn register_arc<T>(&self, tool: Arc<T>) -> Option<Arc<dyn ToolHandle>>
    where
        T: Tool + std::fmt::Debug + 'static,
    {
        let id = tool.id();
        let handle: Arc<dyn ToolHandle> = Arc::new(ErasedTool::from_arc(tool));
        self.inner.entries.write().insert(id, handle)
    }

    /// Resolve `tool_id` to its in-process handle, if registered.
    /// Returns a clone of the `Arc<dyn ToolHandle>` so the
    /// caller can read without holding the lock across an await point.
    pub fn find(&self, tool_id: &ToolId) -> Option<Arc<dyn ToolHandle>> {
        self.inner.entries.read().get(tool_id).cloned()
    }

    /// Drop the handle bound to `tool_id`. Returns `true` iff a
    /// matching entry was removed.
    pub fn unregister(&self, tool_id: &ToolId) -> bool {
        self.inner.entries.write().shift_remove(tool_id).is_some()
    }

    /// Number of tools currently registered.
    pub fn len(&self) -> usize {
        self.inner.entries.read().len()
    }

    /// `true` iff no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.inner.entries.read().is_empty()
    }

    /// `true` iff `tool_id` is currently registered.
    pub fn contains(&self, tool_id: &ToolId) -> bool {
        self.inner.entries.read().contains_key(tool_id)
    }

    /// Register `alias_id` as an alias pointing to the same handle as
    /// `target_id`. Returns `true` if the alias was created (i.e. the
    /// target exists), `false` otherwise.
    ///
    /// Used for MCP prefix-fallback: the model may emit the bare remote
    /// name (`search_channels`) instead of the full prefixed name
    /// (`slack___search_channels`). Registering the bare name as an
    /// alias lets `find` resolve it without prefix-scanning logic.
    pub fn register_alias(&self, alias_id: ToolId, target_id: &ToolId) -> bool {
        if let Some(handle) = self.find(target_id) {
            if let Some(extractor) = self.inner.extractors.get(target_id) {
                self.inner
                    .extractors
                    .insert(alias_id.clone(), extractor.clone());
            }
            self.inner.entries.write().insert(alias_id, handle);
            true
        } else {
            false
        }
    }

    /// Register a type-erased [`ToolDyn`] directly.
    ///
    /// Use this for inherently dynamic tools (e.g. MCP tools retrieved
    /// from a registry as `Arc<dyn ToolDyn>`) where the concrete type
    /// is not available. For native tools with a concrete type, prefer
    /// [`register`](Self::register).
    pub fn register_dyn(
        &self,
        tool: Arc<dyn echo_agent_tool_runtime::ToolDyn>,
    ) -> Option<Arc<dyn ToolHandle>> {
        let id = tool.id();
        // ToolDyn already implements ToolHandle via the blanket impl
        // in echo-agent-computer-hub-core (ErasedTool). We wrap it in a thin
        // adapter that delegates execute → ToolDyn::execute.
        let handle: Arc<dyn ToolHandle> = Arc::new(DynToolAdapter(tool));
        self.inner.entries.write().insert(id, handle)
    }

    /// Register a tool and capture a type-safe model output extractor.
    ///
    /// Equivalent to calling [`register`](Self::register) followed by
    /// [`register_extractor`](Self::register_extractor) with an extractor
    /// built from `T::Output`.
    pub fn register_with_model_output<T>(&self, tool: T) -> Option<Arc<dyn ToolHandle>>
    where
        T: Tool + std::fmt::Debug + 'static,
        T::Output: echo_agent_tool_runtime::ToolOutput + serde::de::DeserializeOwned + 'static,
    {
        let id = tool.id();
        self.inner
            .extractors
            .insert(id, extractor_for::<T::Output>());
        self.register(tool)
    }

    /// Attach a model output extractor for `tool_id`. Replaces any previous.
    pub fn register_extractor(&self, tool_id: ToolId, extractor: ModelOutputExtractor) {
        self.inner.extractors.insert(tool_id, extractor);
    }

    /// Extract model-facing content blocks from a tool's output.
    ///
    /// Returns `None` if no extractor is registered for `tool_id` or if
    /// the value fails to deserialize into the expected output type.
    pub fn model_output(
        &self,
        tool_id: &ToolId,
        output: &Value,
    ) -> Option<Vec<echo_agent_tool_runtime::ContentBlock>> {
        self.inner
            .extractors
            .get(tool_id)
            .and_then(|e| e.value()(output))
    }

    /// Descriptions of registered tools filtered by `should_list`.
    ///
    /// Returns descriptions in **insertion order** — the order tools
    /// were registered — so the caller sees the same ordering as the
    /// config-defined tool list.
    pub fn list_tools(&self, ctx: &ListToolsContext) -> Vec<ToolDescription> {
        self.inner
            .entries
            .read()
            .values()
            .filter(|handle| handle.should_list(ctx))
            .map(|handle| handle.description(ctx))
            .collect()
    }
}

/// Thin adapter from `Arc<dyn ToolDyn>` to `ToolHandle`.
///
/// `ToolDyn::execute` returns `ToolStream<TypedToolOutput>` which matches
/// `ToolHandle::execute`, so the adapter is a trivial delegation.
struct DynToolAdapter(Arc<dyn echo_agent_tool_runtime::ToolDyn>);

impl std::fmt::Debug for DynToolAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynToolAdapter")
            .field("id", &self.0.id())
            .finish()
    }
}

#[async_trait::async_trait]
impl ToolHandle for DynToolAdapter {
    fn id(&self) -> ToolId {
        self.0.id()
    }
    fn description(&self, ctx: &ListToolsContext) -> ToolDescription {
        self.0.description(ctx)
    }
    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        self.0.capabilities()
    }
    fn should_list(&self, ctx: &ListToolsContext) -> bool {
        self.0.should_list(ctx)
    }
    async fn execute(
        &self,
        ctx: ToolCallContext,
        args: Value,
    ) -> ToolStream<echo_agent_tool_runtime::TypedToolOutput> {
        self.0.execute(ctx, args).await
    }
}

/// Builder for [`ToolHarness`]. See module docs for end-to-end usage.
#[derive(Default)]
pub struct ToolHarnessBuilder {
    pool: Option<Arc<HubConnectionPool>>,
    url: Option<Url>,
    auth: Option<Arc<dyn AuthProvider>>,
    session: Option<SessionId>,
    local_registry: LocalRegistry,
    default_extensions: Option<echo_agent_tool_runtime::TypedExtensions>,
    trace_context_provider: Option<TraceContextProvider>,
    on_reconnect: Option<Arc<ReconnectCallback>>,
    /// Sampler label for `hub_harness_connect_total` metric
    /// (`"chat"` or `"shell"`). Defaults to `"unknown"`.
    sampler: Option<String>,
    alpha_test_key: Option<String>,
    allow_insecure_ws: bool,
    /// Resume the build-time `session.open`; default `false`. Does not affect
    /// the transport auto-reconnect loop, which always uses `resume: false`.
    resume: bool,
    last_seq: Option<echo_agent_tool_protocol::LastSeq>,
}

impl ToolHarnessBuilder {
    /// Attach an extra access header on every (re)connect.
    pub fn alpha_test_key(mut self, key: impl Into<String>) -> Self {
        self.alpha_test_key = Some(key.into());
        self
    }

    /// Permit plaintext `ws://` to a non-loopback host. Only enable
    /// when the transport is otherwise secured (e.g. a private network
    /// or TLS-terminating proxy) — the bearer would otherwise cross the
    /// wire in cleartext.
    pub fn allow_insecure_ws(mut self, allow: bool) -> Self {
        self.allow_insecure_ws = allow;
        self
    }

    /// Connection pool to attach to. Required.
    pub fn pool(mut self, pool: Arc<HubConnectionPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Server URL (`ws://` / `wss://`). Required.
    pub fn url(mut self, url: Url) -> Self {
        self.url = Some(url);
        self
    }

    pub fn auth(mut self, cred: AuthCredential) -> Self {
        self.auth = Some(Arc::new(cred));
        self
    }

    pub fn auth_provider(mut self, provider: Arc<dyn AuthProvider>) -> Self {
        self.auth = Some(provider);
        self
    }

    /// Bind `session_id` on the underlying connection and use it as
    /// the envelope `session_id` for outgoing `tool.call` requests.
    /// Calling repeatedly replaces the previous binding.
    pub fn session(mut self, session_id: SessionId) -> Self {
        self.session = Some(session_id);
        self
    }

    /// Register an in-process tool. Additive: subsequent calls add
    /// more tools to the same [`LocalRegistry`].
    pub fn local_tool<T>(self, tool: T) -> Self
    where
        T: Tool + std::fmt::Debug + 'static,
    {
        self.local_registry.register(tool);
        self
    }

    pub fn local_registry(mut self, registry: LocalRegistry) -> Self {
        self.local_registry = registry;
        self
    }

    /// Default extensions merged into every `ToolCallContext` before dispatch.
    pub fn default_extensions(
        mut self,
        extensions: echo_agent_tool_runtime::TypedExtensions,
    ) -> Self {
        self.default_extensions = Some(extensions);
        self
    }

    /// Sampled per outgoing tool call / hook on the caller's task.
    /// Keeps the host's tracing stack out of the SDK.
    pub fn trace_context_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.trace_context_provider = Some(Arc::new(provider));
        self
    }

    /// Optional callback fired once per successful reconnect cycle.
    pub fn on_reconnect<F>(mut self, cb: F) -> Self
    where
        F: Fn(ReconnectEvent) + Send + Sync + 'static,
    {
        self.on_reconnect = Some(Arc::new(Box::new(cb) as ReconnectCallback));
        self
    }

    /// Sampler label for the `hub_harness_connect_total` metric.
    /// Pass `"chat"` or `"shell"` to identify the calling sampler.
    pub fn sampler(mut self, sampler: impl Into<String>) -> Self {
        self.sampler = Some(sampler.into());
        self
    }

    /// Resume the build-time `session.open` (e.g. a cloud reconnect re-attaching
    /// to its existing server session). Default `false`; auto-reconnect is unaffected.
    pub fn resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    /// Last-seen `(connection_id, seq)` paired with [`Self::resume`] for replay dedup.
    pub fn last_seq(mut self, last_seq: echo_agent_tool_protocol::LastSeq) -> Self {
        self.last_seq = Some(last_seq);
        self
    }

    /// Resolve the pool entry, refcount-bind the session, and return
    /// a [`ToolHarness`] ready to dispatch tool calls. On a session-
    /// bind failure the previously-bound sessions are rolled back
    /// before propagating the original error; the pooled connection
    /// stays in the pool for future borrowers (the failed build's
    /// local `Arc<HubConnection>` is dropped, but the pool keeps its
    /// own clone).
    pub async fn build(self) -> Result<ToolHarness, ClientError> {
        let pool = self
            .pool
            .ok_or_else(|| ClientError::InvalidConfig("missing pool".to_owned()))?;
        let url = self
            .url
            .ok_or_else(|| ClientError::InvalidConfig("missing url".to_owned()))?;
        let auth = self
            .auth
            .ok_or_else(|| ClientError::InvalidConfig("missing auth".to_owned()))?;
        let session = self
            .session
            .ok_or_else(|| ClientError::InvalidConfig("missing session".to_owned()))?;
        let sampler = self.sampler.as_deref().unwrap_or("unknown");
        let borrow = match ConnectionBorrow::acquire(
            pool,
            url,
            auth,
            ConnectionKind::Harness,
            self.on_reconnect.clone(),
            None, // on_disconnect (unused for harness connections)
            None, // on_connect (unused for harness connections)
            None, // on_terminal_close (unused for harness connections)
            None,
            None,
            None,
            self.alpha_test_key.clone(),
            self.allow_insecure_ws,
            crate::connection::ConnectionTuning::default(),
        )
        .await
        {
            Ok(b) => {
                crate::metrics::harness_connect("ok", sampler);
                b
            }
            Err(err) => {
                crate::metrics::harness_connect("error", sampler);
                return Err(err);
            }
        };
        // Track the harness session locally for reconnect replay.
        borrow.connection().track_session(session.clone());

        // Register the session on the server so session-scoped RPCs
        // (session_bind_server, etc.) are accepted. The reconnect path
        // already replays session_open per tracked session (connection.rs),
        // but the initial connect must do it explicitly.
        {
            let connection = borrow.connection();
            let request_id = connection.try_alloc_request_id()?;
            let req = echo_agent_tool_protocol::JsonRpcRequest {
                jsonrpc: echo_agent_tool_protocol::JsonRpcVersion,
                id: echo_agent_tool_protocol::JsonRpcId::from_request_id(&request_id),
                session_id: Some(session.clone()),
                method: echo_agent_tool_protocol::Method::SessionOpen
                    .as_wire_str()
                    .to_owned(),
                params: echo_agent_tool_protocol::SessionOpenParams {
                    resume: self.resume,
                    last_seq: self.last_seq,
                },
            };
            if let Err(e) = connection.call_request(request_id, &req).await {
                // Roll back the local track so a later successful harness for
                // this session can still reach the last-borrower untrack edge.
                let _ = connection.untrack_session(&session);
                tracing::warn!(error = %e, "session_open failed during harness build");
                return Err(e);
            }
        }

        let inner = Arc::new(ToolHarnessInner {
            borrow: Some(borrow),
            local_registry: self.local_registry,
            session,
            default_extensions: self.default_extensions.unwrap_or_default(),
            trace_context_provider: self.trace_context_provider,
            remote_tools: arc_swap::ArcSwap::from_pointee(Vec::new()),
            last_bind_report: arc_swap::ArcSwapOption::empty(),
            discovery_handle: parking_lot::Mutex::new(None),
            session_inbox_tx: parking_lot::Mutex::new(None),
            pending_bind: None,
            hook_request_handler: Arc::new(parking_lot::Mutex::new(None)),
        });
        Ok(ToolHarness { inner })
    }
}

/// Typed bind-contract report from a `session.bind` response.
#[derive(Debug, Clone, Default)]
pub struct SessionBindReport {
    /// Version of the tool-server binary that served the bind.
    pub binary_version: Option<String>,
    /// Configured tool ids the server could not serve.
    pub unserved_tool_ids: Vec<String>,
    /// Server-stated reason the toolset resolution failed closed (the bind
    /// advertises no model-facing tools by design when set).
    pub resolve_error: Option<String>,
    /// Advisory image capability tokens from the bind reply. Empty means
    /// unknown, as does any set lacking
    /// [`echo_agent_tool_protocol::IMAGE_CAPABILITIES_V1`].
    pub image_capabilities: Vec<String>,
}

impl From<&echo_agent_tool_protocol::SessionBindServerResult> for SessionBindReport {
    /// Canonical projection of a bind reply onto its report fields; every
    /// bind-contract field is copied here and nowhere else.
    fn from(result: &echo_agent_tool_protocol::SessionBindServerResult) -> Self {
        Self {
            binary_version: result.binary_version.clone(),
            unserved_tool_ids: result.unserved_tool_ids.clone(),
            resolve_error: result.resolve_error.clone(),
            image_capabilities: result.image_capabilities.clone(),
        }
    }
}

/// Harness attached to a pooled [`HubConnection`].
///
/// `ToolHarness` is `Clone`-cheap (`Arc` bump). Cooperative teardown via
/// [`Self::shutdown`] is preferred. Cleanup is **synchronous** and runs at
/// most once across all clones via a shared CAS: `shutdown()`, wrapper
/// `Drop` (best-effort while other clones exist), and `ToolHarnessInner::Drop`
/// (at true refcount-zero) all call the same path. No Tokio runtime is
/// required for Drop teardown.
pub struct ToolHarness {
    inner: Arc<ToolHarnessInner>,
}

/// An owned, type-erased server-bind future, resolving to the server-connected
/// [`ToolHarness`] (or a stringified bind error).
type BindFuture = BoxFuture<'static, Result<ToolHarness, Arc<str>>>;

/// Cloneable handle to the deferred server bind; every clone observes the same
/// single bind, resolving to the server-connected [`ToolHarness`].
type PendingBind = Shared<BindFuture>;

/// Spawn `bind` on the runtime as a cloneable [`PendingBind`], projecting a
/// task-join panic into the bind's `Err`. Shared by the eager constructor and
/// `LazyBind::start` so the two spawn paths can't drift.
fn spawn_pending_bind<F>(bind: F) -> PendingBind
where
    F: std::future::Future<Output = Result<ToolHarness, Arc<str>>> + Send + 'static,
{
    let task = echo_agent_tracing::tokio::spawn_traced(bind);
    async move {
        match task.await {
            Ok(result) => result,
            Err(join_err) => Err(Arc::<str>::from(
                format!("server bind task panicked: {join_err}").as_str(),
            )),
        }
    }
    .boxed()
    .shared()
}

/// A bind future kept unspawned until the first [`ToolHarness::await_bound`], so
/// the server connection — and the sandbox provisioning it performs — is deferred
/// to the first remote tool dispatch. See [`ToolHarness::local_with_lazy_bind`].
struct LazyBind {
    fut: parking_lot::Mutex<Option<BindFuture>>,
    started: std::sync::OnceLock<PendingBind>,
}

impl LazyBind {
    /// Spawn the bind exactly once and return the cloneable shared handle.
    fn start(&self) -> PendingBind {
        self.started
            .get_or_init(|| {
                let fut = self
                    .fut
                    .lock()
                    .take()
                    .expect("LazyBind future taken more than once");
                spawn_pending_bind(fut)
            })
            .clone()
    }
}

/// Deferred server bind: `Eager` is spawned at construction (races sampling);
/// `Lazy` spawns on the first `await_bound` (provisioning deferred to first call).
enum DeferredBind {
    Eager(PendingBind),
    Lazy(LazyBind),
}

struct ToolHarnessInner {
    /// `None` for local-only harnesses (no server connection).
    borrow: Option<ConnectionBorrow>,
    local_registry: LocalRegistry,
    session: SessionId,
    /// Default extensions merged into every `ToolCallContext` before dispatch.
    default_extensions: echo_agent_tool_runtime::TypedExtensions,
    /// See [`ToolHarnessBuilder::trace_context_provider`].
    trace_context_provider: Option<TraceContextProvider>,
    remote_tools: arc_swap::ArcSwap<Vec<ToolDescription>>,
    last_bind_report: arc_swap::ArcSwapOption<SessionBindReport>,
    discovery_handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Weak handle to the demux session-inbox sender this harness registered.
    /// Identity-guarded unregister uses this without holding a strong sender
    /// that would keep the inbox alive after a peer rebind replaces it.
    session_inbox_tx:
        parking_lot::Mutex<Option<tokio::sync::mpsc::WeakSender<crate::demux::InboundFrame>>>,
    /// Deferred server bind (prompt-before-bind): set when this local-only harness
    /// resolves to a server-connected one once the bind completes. Eager variant
    /// races sampling; lazy variant defers provisioning to the first remote
    /// tool dispatch.
    pending_bind: Option<DeferredBind>,
    /// Optional sink for inbound reverse-direction hook requests. Held in
    /// its own `Arc` so the inbox loop can clone this slot — not the whole
    /// `inner` — a long-lived `inner` clone would pin the harness forever and
    /// prevent both the wrapper Drop gate and `ToolHarnessInner::Drop`.
    hook_request_handler: Arc<parking_lot::Mutex<Option<HookRequestHandler>>>,
}

impl ToolHarnessInner {
    /// When the SDK sees a workspace `Disconnected` notification on its own
    /// socket, fail this session's parked `call` futures with the recognizable
    /// `workspace_unavailable` error (`phase: InFlightCancelled`). Client-side
    /// guarantee that closes the window where the server-side cancel is missed
    /// (no subscription / lost broadcast).
    fn fail_inflight_calls_on_disconnect(&self, session_id: &SessionId) {
        let Some(borrow) = self.borrow.as_ref() else {
            return; // local-only harness has no wire calls to fail
        };
        let resolved = borrow
            .connection()
            .demux()
            .fail_calls_for_session(session_id, || {
                ClientError::Wire(workspace_unavailable_wire(
                    WorkspaceGoneReason::Disconnect,
                    WorkspaceGonePhase::InFlightCancelled,
                ))
            });
        if resolved > 0 {
            tracing::info!(
                %session_id,
                resolved,
                "SDK short-circuit: failed in-flight calls on workspace disconnect"
            );
        }
    }

    async fn refresh_remote_tools(&self) -> Result<Vec<ToolDescription>, ClientError> {
        let borrow = self.borrow.as_ref().ok_or_else(|| {
            ClientError::InvalidConfig("local-only harness has no server connection".to_owned())
        })?;
        let tools = list_remote_tools(borrow.connection().as_ref(), &self.session).await?;
        self.remote_tools.store(Arc::new(tools.clone()));
        Ok(tools)
    }

    /// Wins `begin_teardown` then runs cleanup. Idempotent. Synchronous so
    /// Drop cannot strand cleanup on an unpolled spawn.
    fn finish_teardown(&self) {
        let Some(borrow) = self.borrow.as_ref() else {
            return;
        };
        if !borrow.begin_teardown() {
            return;
        }
        if let Some(h) = self.discovery_handle.lock().take() {
            h.abort();
        }
        borrow.shutdown_token().cancel();
        let inbox_tx = self.session_inbox_tx.lock().take();
        release_session_binding(borrow.connection(), &self.session, inbox_tx.as_ref());
    }
}

/// Bound `tools.list` so discovery cannot pin a connection on a hung RPC.
const TOOLS_LIST_TIMEOUT: Duration = Duration::from_secs(30);

async fn list_remote_tools(
    connection: &HubConnection,
    session: &SessionId,
) -> Result<Vec<ToolDescription>, ClientError> {
    let request_id = connection.try_alloc_request_id()?;
    let params = echo_agent_tool_protocol::ToolsListParams {
        session_id: session.clone(),
        mode: echo_agent_tool_protocol::ToolDefinitionMode::Full,
    };
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: Some(session.clone()),
        method: Method::ToolsList.as_wire_str().to_owned(),
        params,
    };
    let resp = connection
        .call_request_with_timeout(request_id, &req, TOOLS_LIST_TIMEOUT)
        .await?;
    match resp.outcome {
        ResponseOutcome::Result(value) => {
            let result: echo_agent_tool_protocol::ToolsListResult =
                serde_json::from_value(value).map_err(|e| ClientError::Serde(e.to_string()))?;
            Ok(result.tools)
        }
        ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
    }
}

/// Untrack; if last borrower, identity-unregister our inbox only.
fn release_session_binding(
    connection: &HubConnection,
    session: &SessionId,
    inbox_tx: Option<&tokio::sync::mpsc::WeakSender<crate::demux::InboundFrame>>,
) {
    if connection.untrack_session(session) != Some(0) {
        return;
    }
    if let Some(weak) = inbox_tx {
        let _ = connection
            .demux()
            .unregister_session_inbox_if_weak(session, weak);
    }
}

impl Drop for ToolHarnessInner {
    fn drop(&mut self) {
        self.finish_teardown();
    }
}

impl Clone for ToolHarness {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl std::fmt::Debug for ToolHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolHarness")
            .field("session", &self.inner.session)
            .field("local_tool_count", &self.inner.local_registry.len())
            .finish_non_exhaustive()
    }
}

impl ToolHarness {
    /// Construct a local-only harness (no server connection).
    ///
    /// Tools are resolved exclusively from the `LocalRegistry`. Remote
    /// dispatch returns `ToolError::NotFound`. `default_extensions` are
    /// merged into every `ToolCallContext` before dispatch — use this
    /// to inject `EchoAgentSharedState`, `EchoAgentAgentState`, etc.
    pub fn local_only_with(
        registry: LocalRegistry,
        session: SessionId,
        default_extensions: echo_agent_tool_runtime::TypedExtensions,
    ) -> Self {
        let inner = Arc::new(ToolHarnessInner {
            borrow: None,
            local_registry: registry,
            session,
            default_extensions,
            trace_context_provider: None,
            remote_tools: arc_swap::ArcSwap::from_pointee(Vec::new()),
            last_bind_report: arc_swap::ArcSwapOption::empty(),
            discovery_handle: parking_lot::Mutex::new(None),
            session_inbox_tx: parking_lot::Mutex::new(None),
            pending_bind: None,
            hook_request_handler: Arc::new(parking_lot::Mutex::new(None)),
        });
        Self { inner }
    }

    /// Construct a local-only harness whose server bind is deferred to a background
    /// task, spawned eagerly so the connection races with sampling instead of
    /// blocking it. Local tools dispatch immediately; remote work awaits
    /// [`Self::await_bound`] (or probes [`Self::try_bound`]).
    pub fn local_with_pending_bind<F>(
        registry: LocalRegistry,
        session: SessionId,
        default_extensions: echo_agent_tool_runtime::TypedExtensions,
        bind: F,
    ) -> Self
    where
        F: std::future::Future<Output = Result<ToolHarness, Arc<str>>> + Send + 'static,
    {
        let pending = spawn_pending_bind(bind);
        let inner = Arc::new(ToolHarnessInner {
            borrow: None,
            local_registry: registry,
            session,
            default_extensions,
            trace_context_provider: None,
            remote_tools: arc_swap::ArcSwap::from_pointee(Vec::new()),
            last_bind_report: arc_swap::ArcSwapOption::empty(),
            discovery_handle: parking_lot::Mutex::new(None),
            session_inbox_tx: parking_lot::Mutex::new(None),
            pending_bind: Some(DeferredBind::Eager(pending)),
            hook_request_handler: Arc::new(parking_lot::Mutex::new(None)),
        });
        Self { inner }
    }

    /// Construct a local-only harness whose server bind is deferred *and not
    /// started* until the first [`Self::await_bound`] call. Unlike
    /// [`Self::local_with_pending_bind`], the bind future is stored unspawned,
    /// so the server connection — and the sandbox provisioning it triggers — only
    /// begins on the first remote tool dispatch. Local tools dispatch
    /// immediately and never start the bind; [`Self::try_bound`] returns `None`
    /// until the bind has been started (it does not itself kick it off).
    pub fn local_with_lazy_bind<F>(
        registry: LocalRegistry,
        session: SessionId,
        default_extensions: echo_agent_tool_runtime::TypedExtensions,
        bind: F,
    ) -> Self
    where
        F: std::future::Future<Output = Result<ToolHarness, Arc<str>>> + Send + 'static,
    {
        let lazy = LazyBind {
            fut: parking_lot::Mutex::new(Some(bind.boxed())),
            started: std::sync::OnceLock::new(),
        };
        let inner = Arc::new(ToolHarnessInner {
            borrow: None,
            local_registry: registry,
            session,
            default_extensions,
            trace_context_provider: None,
            remote_tools: arc_swap::ArcSwap::from_pointee(Vec::new()),
            last_bind_report: arc_swap::ArcSwapOption::empty(),
            discovery_handle: parking_lot::Mutex::new(None),
            session_inbox_tx: parking_lot::Mutex::new(None),
            pending_bind: Some(DeferredBind::Lazy(lazy)),
            hook_request_handler: Arc::new(parking_lot::Mutex::new(None)),
        });
        Self { inner }
    }

    pub fn has_pending_bind(&self) -> bool {
        self.inner.pending_bind.is_some()
    }

    /// Await the deferred server bind and return the server-connected harness, or a
    /// clone of `self` when there is no pending bind (so callers dispatch
    /// through the result uniformly).
    ///
    /// For a lazy bind (see [`Self::local_with_lazy_bind`]), this is what
    /// actually starts the bind — the first call spawns it.
    pub async fn await_bound(&self) -> Result<ToolHarness, Arc<str>> {
        match &self.inner.pending_bind {
            Some(DeferredBind::Eager(pending)) => pending.clone().await,
            Some(DeferredBind::Lazy(lazy)) => lazy.start().await,
            None => Ok(self.clone()),
        }
    }

    /// Non-blocking probe of the deferred bind: `None` while in flight (or no
    /// pending bind), `Some(Ok)`/`Some(Err)` once resolved. Uses `now_or_never`
    /// (not `peek`): the bind runs in a spawned task, so `Shared` only observes
    /// completion once polled.
    ///
    /// For a lazy bind, this never *starts* the bind — it returns `None` until
    /// a prior [`Self::await_bound`] call has spawned it, then probes that.
    pub fn try_bound(&self) -> Option<Result<ToolHarness, Arc<str>>> {
        match self.inner.pending_bind.as_ref()? {
            DeferredBind::Eager(pending) => pending.clone().now_or_never(),
            DeferredBind::Lazy(lazy) => lazy.started.get()?.clone().now_or_never(),
        }
    }

    /// Underlying connection. Useful for tests that need to assert
    /// pool dedup.
    pub fn connection(&self) -> Result<&Arc<HubConnection>, ClientError> {
        self.require_connection()
    }

    /// Bound session.
    pub fn session(&self) -> &SessionId {
        &self.inner.session
    }

    pub fn local_registry(&self) -> LocalRegistry {
        self.inner.local_registry.clone()
    }

    /// Extract model-facing content blocks from a tool's output.
    ///
    /// Returns `None` if no extractor is registered for `tool_id`
    /// or if deserialization fails.
    pub fn model_output(
        &self,
        tool_id: &ToolId,
        output: &Value,
    ) -> Option<Vec<echo_agent_tool_runtime::ContentBlock>> {
        self.inner.local_registry.model_output(tool_id, output)
    }

    fn require_connection(&self) -> Result<&Arc<HubConnection>, ClientError> {
        self.inner
            .borrow
            .as_ref()
            .map(|b| b.connection())
            .ok_or_else(|| {
                ClientError::InvalidConfig(
                    "operation requires a server connection (local-only harness)".to_owned(),
                )
            })
    }

    /// Discover available tool servers for the current user.
    pub async fn list_servers(
        &self,
    ) -> Result<Vec<echo_agent_tool_protocol::ServerInfo>, ClientError> {
        let connection = self.require_connection()?;
        let request_id = connection.try_alloc_request_id()?;
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(self.inner.session.clone()),
            method: echo_agent_tool_protocol::Method::ServersList
                .as_wire_str()
                .to_owned(),
            params: echo_agent_tool_protocol::ServersListParams {},
        };
        let resp = connection.call_request(request_id, &req).await?;
        match resp.outcome {
            ResponseOutcome::Result(value) => {
                let result: echo_agent_tool_protocol::ServersListResult =
                    serde_json::from_value(value).map_err(|e| ClientError::Serde(e.to_string()))?;
                Ok(result.servers)
            }
            ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
        }
    }

    /// Open a session on the server. Does not bind any server.
    ///
    /// Registers the session on the server connection and claims
    /// ownership. Server binding is a separate step via
    /// [`Self::session_bind`].
    pub async fn session_open(&self) -> Result<(), ClientError> {
        self.session_open_with(false, None).await
    }

    /// [`Self::session_open`] with an explicit `resume` flag and optional `last_seq`.
    pub async fn session_open_with(
        &self,
        resume: bool,
        last_seq: Option<echo_agent_tool_protocol::LastSeq>,
    ) -> Result<(), ClientError> {
        let start = std::time::Instant::now();
        let result: Result<(), ClientError> = async {
            let connection = self.require_connection()?;
            let request_id = connection.try_alloc_request_id()?;
            let params = echo_agent_tool_protocol::SessionOpenParams { resume, last_seq };
            let req = JsonRpcRequest {
                jsonrpc: JsonRpcVersion,
                id: JsonRpcId::from_request_id(&request_id),
                session_id: Some(self.inner.session.clone()),
                method: Method::SessionOpen.as_wire_str().to_owned(),
                params,
            };
            let resp = connection.call_request(request_id, &req).await?;
            match resp.outcome {
                ResponseOutcome::Result(_) => Ok(()),
                ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
            }
        }
        .await;
        crate::metrics::session_op_observe(
            "open",
            if result.is_ok() { "ok" } else { "error" },
            start.elapsed().as_secs_f64(),
        );
        result
    }

    /// Bind a server's tools to the current session.
    ///
    /// Returns the tools available after the server is bound. Updates
    /// the in-memory remote tools snapshot. Optional `cwd` and
    /// `metadata` are forwarded to the tool server's session creation.
    pub async fn session_bind(
        &self,
        server_id: &str,
        cwd: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<Vec<ToolDescription>, ClientError> {
        self.session_bind_with_report(server_id, cwd, metadata)
            .await
            .map(|r| r.tools)
    }

    /// [`Self::session_bind`], returning the full bind result including the
    /// server's bind report.
    pub async fn session_bind_with_report(
        &self,
        server_id: &str,
        cwd: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<echo_agent_tool_protocol::SessionBindServerResult, ClientError> {
        let start = std::time::Instant::now();
        let result: Result<echo_agent_tool_protocol::SessionBindServerResult, ClientError> =
            async {
                let connection = self.require_connection()?;
                let request_id = connection.try_alloc_request_id()?;
                let parsed_server_id = echo_agent_tool_protocol::ServerId::new(server_id)
                    .map_err(|e| ClientError::InvalidConfig(format!("invalid server_id: {e}")))?;
                let params = echo_agent_tool_protocol::SessionBindServerParams {
                    server_id: parsed_server_id,
                    cwd: cwd.map(String::from),
                    metadata,
                };
                let req = JsonRpcRequest {
                    jsonrpc: JsonRpcVersion,
                    id: JsonRpcId::from_request_id(&request_id),
                    session_id: Some(self.inner.session.clone()),
                    method: Method::SessionBindServer.as_wire_str().to_owned(),
                    params,
                };
                let resp = connection.call_request(request_id, &req).await?;
                match resp.outcome {
                    ResponseOutcome::Result(value) => {
                        let bind_result: echo_agent_tool_protocol::SessionBindServerResult =
                            serde_json::from_value(value)
                                .map_err(|e| ClientError::Serde(e.to_string()))?;
                        let arc = Arc::new(bind_result.tools.clone());
                        self.inner.remote_tools.store(arc);
                        self.inner
                            .last_bind_report
                            .store(Some(Arc::new(SessionBindReport::from(&bind_result))));
                        Ok(bind_result)
                    }
                    ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
                }
            }
            .await;
        crate::metrics::session_op_observe(
            "bind",
            if result.is_ok() { "ok" } else { "error" },
            start.elapsed().as_secs_f64(),
        );
        result
    }

    /// Attach to the current session as an observer
    /// (`session_attach_server`): a server-local check that a tool-server is
    /// routed for the envelope session, replying the tool snapshot and the
    /// route it was found on. Never binds, never creates a workspace
    /// session, never touches toolsets or handlers. Updates the in-memory
    /// remote tools snapshot like [`Self::session_bind`].
    ///
    /// `server_id` is an optional diagnostics cross-check (the envelope
    /// session is the authoritative key); `caller` is a free-form label
    /// surfaced in server metrics/logs. An attach miss is the retryable
    /// `workspace_unavailable` family (`reason: not_bound`). Server support
    /// is answered before calling, not from the reply: the server advertises
    /// `session_attach_server` in `hello_ack` (capability and method ship
    /// in the same server commit) — gate on `HubConnection::supports`; a server
    /// that predates the method advertises nothing and replies `-32601`.
    pub async fn session_attach(
        &self,
        server_id: Option<&str>,
        caller: &str,
    ) -> Result<echo_agent_tool_protocol::SessionAttachServerResult, ClientError> {
        let start = std::time::Instant::now();
        let result: Result<echo_agent_tool_protocol::SessionAttachServerResult, ClientError> =
            async {
                let connection = self.require_connection()?;
                let request_id = connection.try_alloc_request_id()?;
                let parsed_server_id = server_id
                    .map(|s| {
                        echo_agent_tool_protocol::ServerId::new(s).map_err(|e| {
                            ClientError::InvalidConfig(format!("invalid server_id: {e}"))
                        })
                    })
                    .transpose()?;
                let params = echo_agent_tool_protocol::SessionAttachServerParams {
                    server_id: parsed_server_id,
                    caller: Some(caller.to_owned()),
                };
                let req = JsonRpcRequest {
                    jsonrpc: JsonRpcVersion,
                    id: JsonRpcId::from_request_id(&request_id),
                    session_id: Some(self.inner.session.clone()),
                    method: Method::SessionAttachServer.as_wire_str().to_owned(),
                    params,
                };
                let resp = connection.call_request(request_id, &req).await?;
                match resp.outcome {
                    ResponseOutcome::Result(value) => {
                        let attach_result: echo_agent_tool_protocol::SessionAttachServerResult =
                            serde_json::from_value(value)
                                .map_err(|e| ClientError::Serde(e.to_string()))?;
                        self.inner
                            .remote_tools
                            .store(Arc::new(attach_result.tools.clone()));
                        Ok(attach_result)
                    }
                    ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
                }
            }
            .await;
        crate::metrics::session_op_observe(
            "attach",
            if result.is_ok() { "ok" } else { "error" },
            start.elapsed().as_secs_f64(),
        );
        result
    }

    /// Unbind a server from the current session.
    pub async fn session_unbind(&self, server_id: &str) -> Result<(), ClientError> {
        let connection = self.require_connection()?;
        let request_id = connection.try_alloc_request_id()?;
        let parsed_server_id = echo_agent_tool_protocol::ServerId::new(server_id)
            .map_err(|e| ClientError::InvalidConfig(format!("invalid server_id: {e}")))?;
        let params = echo_agent_tool_protocol::SessionUnbindServerParams {
            server_id: parsed_server_id,
        };
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(self.inner.session.clone()),
            method: Method::SessionUnbindServer.as_wire_str().to_owned(),
            params,
        };
        let resp = connection.call_request(request_id, &req).await?;
        match resp.outcome {
            ResponseOutcome::Result(_) => {
                // Clear cached remote tools — the server's tools are no
                // longer available after unbind.
                self.inner.remote_tools.store(Arc::new(Vec::new()));
                Ok(())
            }
            ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
        }
    }

    /// Close a session, unbinding all servers.
    ///
    /// On the server side, this drops all tool bindings for the session
    /// and sends `session.unbind` to any bound tool servers.
    pub async fn session_close(&self) -> Result<(), ClientError> {
        let connection = self.require_connection()?;
        let request_id = connection.try_alloc_request_id()?;
        let params = echo_agent_tool_protocol::SessionCloseParams { reason: None };
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(self.inner.session.clone()),
            method: Method::SessionClose.as_wire_str().to_owned(),
            params,
        };
        let resp = connection.call_request(request_id, &req).await?;
        match resp.outcome {
            ResponseOutcome::Result(_) => Ok(()),
            ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
        }
    }

    /// All tool descriptions: local registry plus cached remote tools.
    pub fn list_tools(&self, ctx: &ListToolsContext) -> Vec<ToolDescription> {
        let mut tools = self.list_local_tools(ctx);
        tools.extend(self.list_remote_tools());
        tools
    }

    /// Whether `name` is a known remote tool, per the cached
    /// `session_bind` / `tools.list` result. Empty before the first
    /// bind/discovery and after unbind; always `false` for local-only
    /// harnesses. Remote tools are not mirrored into the local registry.
    pub fn has_remote_tool(&self, name: &str) -> bool {
        self.inner
            .remote_tools
            .load()
            .iter()
            .any(|t| t.name == name)
    }

    /// Test-only: seed the remote-tools cache without a server round-trip.
    #[doc(hidden)]
    pub fn seed_remote_tools_for_tests(&self, tools: Vec<ToolDescription>) {
        self.inner.remote_tools.store(Arc::new(tools));
    }

    /// Test-only: whether `start_tool_discovery` installed a background task.
    #[doc(hidden)]
    pub fn discovery_task_started_for_tests(&self) -> bool {
        self.inner.discovery_handle.lock().is_some()
    }

    /// Tool descriptions from the local registry only.
    pub fn list_local_tools(&self, ctx: &ListToolsContext) -> Vec<ToolDescription> {
        self.inner.local_registry.list_tools(ctx)
    }

    /// Cached tool descriptions advertised by remote tool servers.
    pub fn list_remote_tools(&self) -> Vec<ToolDescription> {
        self.inner.remote_tools.load().iter().cloned().collect()
    }

    /// Shared snapshot of the cached remote tool descriptions, cloning only the
    /// backing `Arc` rather than the `Vec`.
    pub fn remote_tools_snapshot(&self) -> Arc<Vec<ToolDescription>> {
        self.inner.remote_tools.load_full()
    }

    /// The bind-contract report from the most recent successful `session.bind`, if any.
    pub fn last_bind_report(&self) -> Option<Arc<SessionBindReport>> {
        self.inner.last_bind_report.load_full()
    }

    /// Test-only: seed the bind report.
    pub fn seed_bind_report_for_tests(&self, report: SessionBindReport) {
        self.inner.last_bind_report.store(Some(Arc::new(report)));
    }

    /// Dispatch a tool call.
    ///
    /// Local-first: if `tool_id` is registered in the
    /// [`LocalRegistry`] the tool's `execute` runs in-process and the
    /// returned [`ToolStream`] is forwarded verbatim — no wire
    /// round-trip. The hot path on a hit is a single `DashMap` lookup
    /// plus an `Arc` clone of the handle.
    ///
    /// Otherwise the harness sends a `tool.call` JSON-RPC request
    /// over the shared connection. The returned stream interleaves
    /// any intermediate `tool_call_progress` notifications (matched
    /// by `tool_call_id`) with the eventual JSON-RPC response, which
    /// becomes the terminal item.
    ///
    /// Each call MUST use a fresh `ToolCallId`. The default
    /// [`ToolCallContext::default`] mints a UUIDv7 via
    /// [`ToolCallId::new_v7`]; callers that build a context manually
    /// MUST do the same. Reusing a `ToolCallId` already in flight on
    /// this connection is detected synchronously: the second call's
    /// stream resolves with a single `Terminal(Err(_))` carrying
    /// `ToolError::Custom { code: "call_id_in_use", .. }` and the
    /// FIRST call's progress and response correlation are left intact
    /// (no stomp). The `tool_call_id` keys the per-call progress
    /// channel; concurrent ids would otherwise observe each other's
    /// progress frames.
    pub async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        mut ctx: ToolCallContext,
    ) -> ToolStream<echo_agent_tool_runtime::TypedToolOutput> {
        // Merge harness-level default extensions (SharedState, AgentState, etc.)
        // into the per-call context. Per-call values take priority.
        ctx.extensions
            .merge_defaults(&self.inner.default_extensions);

        // Sampled on the caller's task while the dispatching span is
        // active; never rides ctx extensions.
        let trace_context = self
            .inner
            .trace_context_provider
            .as_ref()
            .and_then(|provider| provider());

        // Capture identifiers before `ctx` moves into the dispatch path —
        // they feed both the local-only / remote branches AND the
        // `ObservedToolStream` wrapper.
        let observed_call_id = ctx.call_id.clone();

        let raw_stream: ToolStream<echo_agent_tool_runtime::TypedToolOutput> =
            if let Some(handle) = self.inner.local_registry.find(&tool_id) {
                handle.execute(ctx, args).await
            } else if let Some(ref borrow) = self.inner.borrow {
                let start = std::time::Instant::now();
                let stream = dispatch_remote(
                    borrow.connection(),
                    &self.inner.session,
                    tool_id.clone(),
                    args,
                    ctx,
                    trace_context,
                )
                .await;
                crate::metrics::call_dispatch_observe(start.elapsed().as_secs_f64());
                stream
            } else {
                let message = format!("tool not found (local-only harness): {tool_id}");
                return terminal_only(Err(ToolError::not_found(tool_id, message)));
            };

        // Server observability: only wrap when the harness has a server
        // connection — otherwise emission is a no-op and the extra
        // Started/Completed bookkeeping is pure waste. Local-only
        // harnesses (and the no-`borrow` not-found branch above) skip
        // wrapping entirely.
        if self.inner.borrow.is_none() {
            return raw_stream;
        }
        self.emit_session_event(SessionEvent::ToolCallStarted {
            tool_call_id: observed_call_id.as_str().to_owned(),
            tool_name: tool_id.as_str().to_owned(),
            turn_number: 0,
        })
        .await;
        Box::pin(ObservedToolStream::new(
            raw_stream,
            self.clone(),
            observed_call_id,
            tool_id,
        ))
    }

    /// Emit a session-level event to the server as a `session_event`
    /// custom notification.
    ///
    /// No-op on a local-only harness — without a server connection there is
    /// nothing to deliver to and the wire frame would be discarded. Server
    /// errors are silently ignored: emission is fire-and-forget and must
    /// never affect dispatch.
    ///
    /// `ToolHarness::call` invokes this automatically for
    /// `ToolCallStarted` / `ToolCallCompleted`; higher-level events
    /// (turn lifecycle, phase changes) are emitted by callers that
    /// already hold the harness.
    pub async fn emit_session_event(&self, event: SessionEvent) {
        if self.inner.borrow.is_none() {
            return;
        }
        let _ = self
            .send_notification(build_session_event_frame(&event))
            .await;
    }

    /// Send a `tool.notify` frame to the server.
    ///
    /// The server fans the notification out to every connection that has an
    /// active `subscribe_notifications` subscription for this session.
    /// The frame is fire-and-forget: this method returns `Ok` once the
    /// outbound message is queued, without waiting for a server ack.
    /// Server-side errors (e.g. unknown tool, invalid session) are not
    /// surfaced to the caller.
    pub async fn send_notification(
        &self,
        notification: echo_agent_tool_protocol::ToolNotificationFrame,
    ) -> Result<(), ClientError> {
        self.send_fire_and_forget(Method::ToolNotify, notification)
            .await
    }

    /// Send a `hook` frame to the server.
    ///
    /// The server routes the hook to the tool server that owns the targeted
    /// tool (or broadcasts to all servers bound to the session for
    /// session-wide hooks like `Pause` / `Resume` / `SessionEnded`).
    /// The frame is fire-and-forget: this method returns `Ok` once the
    /// outbound message is queued, without waiting for a server ack.
    /// Server-side errors (e.g. unknown tool, invalid session) are not
    /// surfaced to the caller.
    pub async fn send_hook(
        &self,
        mut hook: echo_agent_tool_protocol::HookFrame,
    ) -> Result<(), ClientError> {
        if hook.trace_context.is_none()
            && let Some(provider) = &self.inner.trace_context_provider
        {
            hook.trace_context = provider();
        }
        let hook_type = match &hook.event {
            echo_agent_tool_protocol::HookEvent::Cancel => "cancel",
            echo_agent_tool_protocol::HookEvent::Pause => "pause",
            echo_agent_tool_protocol::HookEvent::Resume => "resume",
            echo_agent_tool_protocol::HookEvent::SessionEnded => "session_ended",
            echo_agent_tool_protocol::HookEvent::Custom { .. } => "custom",
        };
        crate::metrics::hook_send(hook_type);
        self.send_fire_and_forget(Method::Hook, hook).await
    }

    /// Cancel an in-flight remote call.
    ///
    /// Sends the call-scoped `Cancel` [`HookFrame`](echo_agent_tool_protocol::HookFrame)
    /// over the fire-and-forget [`Self::send_hook`] path. Idempotent: the
    /// server routes it to the owning tool server, which hard-cancels a live
    /// call or tombstones an unknown / already-completed `call_id`. `Ok`
    /// means the frame was queued; a late or repeated id is never an error
    /// here.
    pub async fn cancel_call(
        &self,
        tool_id: &ToolId,
        call_id: &ToolCallId,
    ) -> Result<(), ClientError> {
        let hook = echo_agent_tool_protocol::HookFrame::cancel(
            self.inner.session.clone(),
            tool_id.clone(),
            call_id.clone(),
        );
        self.send_hook(hook).await
    }

    /// Send a `before_turn` hook to all tool servers bound to this session.
    ///
    /// Fire-and-forget: returns `Ok` once the frame is queued on the
    /// outbound channel. The workspace (or any other tool server) receives
    /// the hook via `ToolServerHandler::handle_hook`.
    pub async fn send_before_turn_hook(
        &self,
        payload: echo_agent_tool_protocol::turn_hook::BeforeTurnPayload,
    ) -> Result<(), ClientError> {
        let hook = echo_agent_tool_protocol::HookFrame::custom(
            self.session().clone(),
            echo_agent_tool_protocol::turn_hook::BEFORE_TURN_KIND.to_owned(),
            serde_json::to_value(&payload).map_err(|e| ClientError::Serde(e.to_string()))?,
        );
        self.send_hook(hook).await
    }

    /// Send an `after_turn` hook to all tool servers bound to this session.
    ///
    /// Fire-and-forget: returns `Ok` once the frame is queued on the
    /// outbound channel. The workspace (or any other tool server) receives
    /// the hook via `ToolServerHandler::handle_hook`.
    pub async fn send_after_turn_hook(
        &self,
        payload: echo_agent_tool_protocol::turn_hook::AfterTurnPayload,
    ) -> Result<(), ClientError> {
        let hook = echo_agent_tool_protocol::HookFrame::custom(
            self.session().clone(),
            echo_agent_tool_protocol::turn_hook::AFTER_TURN_KIND.to_owned(),
            serde_json::to_value(&payload).map_err(|e| ClientError::Serde(e.to_string()))?,
        );
        self.send_hook(hook).await
    }

    /// Max time to await a turn-hook reply before giving up.
    pub const TURN_HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Request turn-boundary injections + a loop-control decision from the bound workspace server,
    /// bounded by [`TURN_HOOK_TIMEOUT`](Self::TURN_HOOK_TIMEOUT). Sent as a request/response hook
    /// (not a tool); any error is treated as a no-op by the caller.
    pub async fn request_turn_hook(
        &self,
        request: &echo_agent_tool_protocol::turn_hook::TurnHookRequest,
    ) -> Result<echo_agent_tool_protocol::turn_hook::HookReply, ClientError> {
        self.request_turn_hook_with_timeout(request, Self::TURN_HOOK_TIMEOUT)
            .await
    }

    /// [`Self::request_turn_hook`] with a caller-supplied reply deadline
    /// (the responder's own watchdog must stay below it).
    pub async fn request_turn_hook_with_timeout(
        &self,
        request: &echo_agent_tool_protocol::turn_hook::TurnHookRequest,
        timeout: std::time::Duration,
    ) -> Result<echo_agent_tool_protocol::turn_hook::HookReply, ClientError> {
        let connection = self.require_connection()?;
        let payload = serde_json::to_value(request).map_err(ClientError::from)?;
        // hook_id keys the server's parked-request table — must be globally unique.
        let hook_id = echo_agent_tool_protocol::ToolCallId::new_v7().to_string();
        let hook = echo_agent_tool_protocol::HookFrame::custom_request(
            self.inner.session.clone(),
            hook_id,
            echo_agent_tool_protocol::turn_hook::TURN_HOOK_KIND.to_owned(),
            payload,
        )
        .with_trace_context(
            self.inner
                .trace_context_provider
                .as_ref()
                .and_then(|provider| provider()),
        );
        let request_id = connection.try_alloc_request_id()?;
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(self.inner.session.clone()),
            method: Method::Hook.as_wire_str().to_owned(),
            params: hook,
        };

        // Tag transport errors with turn-hook context; other variants pass through.
        let resp = connection
            .call_request_with_timeout(request_id, &req, timeout)
            .await
            .map_err(|e| match e {
                ClientError::NetworkError(msg) => {
                    ClientError::NetworkError(format!("turn hook: {msg}"))
                }
                other => other,
            })?;

        match resp.outcome {
            ResponseOutcome::Result(value) => {
                serde_json::from_value(value).map_err(|e| ClientError::Serde(e.to_string()))
            }
            ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
        }
    }

    /// Answer a reverse-direction request/response [`HookFrame`](echo_agent_tool_protocol::HookFrame)
    /// (one whose `hook_id` is set).
    ///
    /// Sent as a fire-and-forget `hook_reply` notification (not a JSON-RPC
    /// response): the server correlates it to the parked request by `hook_id`.
    /// `Ok` once the frame is queued on the outbound channel.
    pub async fn send_hook_reply(
        &self,
        reply: echo_agent_tool_protocol::HookReplyFrame,
    ) -> Result<(), ClientError> {
        let connection = self.require_connection()?;
        let notif = build_hook_reply_notification(&self.inner.session, reply);
        let text = serde_json::to_string(&notif).map_err(ClientError::from)?;
        connection.send_outbound(text).await
    }

    /// Best-effort, non-blocking twin of [`Self::send_hook_reply`] for
    /// synchronous `Drop`/teardown paths that cannot `.await`.
    ///
    /// Enqueues via [`HubConnection::try_send_outbound`]; a full or closed
    /// outbound channel returns `Err` and the frame is abandoned — the server's
    /// parked-request backstop then releases the await. Mirrors
    /// `RemoteCallStream`'s cancel-on-drop discipline. The async variant's only
    /// edge over this is a brief bounded wait when the channel is momentarily
    /// full, which best-effort teardown does not need.
    pub fn try_send_hook_reply(
        &self,
        reply: echo_agent_tool_protocol::HookReplyFrame,
    ) -> Result<(), ClientError> {
        let connection = self.require_connection()?;
        let notif = build_hook_reply_notification(&self.inner.session, reply);
        let text = serde_json::to_string(&notif).map_err(ClientError::from)?;
        connection.try_send_outbound(text)
    }

    /// Register the sink for inbound reverse-direction hook requests
    /// (server → harness). Replaces any prior handler; the inbox loop loads
    /// it per frame, so registering before or after
    /// [`Self::subscribe_notifications`] both work.
    ///
    /// `handler` runs **inline** on the shared inbox loop that also delivers
    /// [`HubNotification`](crate::notification::HubNotification)s, so it MUST
    /// NOT block: only enqueue / hand the frame off (e.g. a non-blocking
    /// channel `try_send`) and return promptly. Blocking here stalls
    /// notification delivery for the whole session. A panic is caught (the
    /// frame is dropped and the loop continues), but should still be avoided.
    pub fn set_hook_request_handler<F>(&self, handler: F)
    where
        F: Fn(echo_agent_tool_protocol::HookFrame) + Send + Sync + 'static,
    {
        *self.inner.hook_request_handler.lock() = Some(Arc::new(handler));
    }

    /// Shared implementation for fire-and-forget JSON-RPC requests.
    ///
    /// Allocates a request id, constructs a [`JsonRpcRequest`] with the
    /// given method and params, serializes it, and queues it on the
    /// outbound channel. Used by [`Self::send_notification`] and
    /// [`Self::send_hook`] to avoid duplicating the boilerplate.
    async fn send_fire_and_forget<P: serde::Serialize>(
        &self,
        method: Method,
        params: P,
    ) -> Result<(), ClientError> {
        let connection = self.require_connection()?;
        let (_request_id, text) =
            build_request_frame(connection, &self.inner.session, method, params)?;
        connection.send_outbound(text).await
    }

    /// Subscribe to server notifications for the bound session.
    ///
    /// The server auto-subscribes on `register_session`, so no wire
    /// request is needed. Registers a session inbox on the demux and
    /// spawns a task that parses notifications onto the returned channel
    /// and routes reverse-direction hook requests to the handler set via
    /// [`Self::set_hook_request_handler`].
    pub async fn subscribe_notifications(
        &self,
    ) -> Result<mpsc::Receiver<crate::notification::HubNotification>, ClientError> {
        let connection = self.require_connection()?;
        if self.inner.borrow.as_ref().is_some_and(|b| b.is_torn_down()) {
            return Err(ClientError::InvalidConfig(
                "harness already torn down".to_owned(),
            ));
        }

        let (inbox_tx, mut inbox_rx) = mpsc::channel::<crate::demux::InboundFrame>(64);
        // Weak only — a strong clone would keep the channel open after a peer
        // rebind replaces the demux entry and would block the prior discovery
        // task from seeing EOF. Keep a stack-local weak for undo: concurrent
        // finish_teardown may take the mutex slot without demux-unregistering
        // (non-last untrack), so undo must not rely on that take.
        let inbox_weak = inbox_tx.downgrade();
        connection
            .demux()
            .register_session_inbox(self.inner.session.clone(), inbox_tx);
        *self.inner.session_inbox_tx.lock() = Some(inbox_weak.clone());

        // Teardown may have won between the check and register — undo.
        if self.inner.borrow.as_ref().is_some_and(|b| b.is_torn_down()) {
            let _ = connection
                .demux()
                .unregister_session_inbox_if_weak(&self.inner.session, &inbox_weak);
            *self.inner.session_inbox_tx.lock() = None;
            return Err(ClientError::InvalidConfig(
                "harness already torn down".to_owned(),
            ));
        }

        let (event_tx, event_rx) = mpsc::channel::<crate::notification::HubNotification>(64);
        // Clone only the handler slot (a standalone `Arc`), never `inner`:
        // an `inner` clone here would keep the strong count above 1 and
        // suppress the `Drop` teardown gate.
        let hook_request_handler = self.inner.hook_request_handler.clone();
        tokio::spawn(async move {
            while let Some(frame) = inbox_rx.recv().await {
                match frame {
                    crate::demux::InboundFrame::Notification(value) => {
                        if let Some(event) = crate::notification::HubNotification::parse(&value)
                            && event_tx.send(event).await.is_err()
                        {
                            break;
                        }
                    }
                    crate::demux::InboundFrame::Request(value) => {
                        dispatch_inbound_hook_request(&value, &hook_request_handler);
                    }
                }
            }
        });

        Ok(event_rx)
    }

    /// Query the server for remote tool descriptions via `tools.list` RPC
    /// and store the result in the in-memory cache. Returns the
    /// discovered tools.
    pub async fn query_remote_tools(&self) -> Result<Vec<ToolDescription>, ClientError> {
        self.inner.refresh_remote_tools().await
    }

    /// Start background tool discovery: populate the cache, then
    /// re-query on every `ToolsChanged` notification.
    pub async fn start_tool_discovery(&self) {
        if self.inner.borrow.as_ref().is_some_and(|b| b.is_torn_down()) {
            return;
        }

        let Ok(mut rx) = self.subscribe_notifications().await else {
            tracing::warn!("tool discovery: failed to subscribe to notifications");
            return;
        };
        // Local weak for post-install undo if finish_teardown steals the mutex.
        let inbox_weak = self.inner.session_inbox_tx.lock().clone();

        if let Err(e) = self.query_remote_tools().await {
            tracing::warn!(error = %e, "tool discovery: initial query failed");
        }

        // Capture a Weak so the discovery task never pins ToolHarnessInner
        // (a strong Arc would form a cycle via demux inbox → task → Arc →
        // ConnectionBorrow → HubConnection → demux and defeat Drop teardown).
        let weak = Arc::downgrade(&self.inner);
        let handle = tokio::spawn(async move {
            while let Some(notification) = rx.recv().await {
                let Some(inner) = weak.upgrade() else {
                    break; // harness gone — exit without extending its lifetime
                };
                match notification {
                    crate::notification::HubNotification::ToolsChanged { .. } => {
                        // Drop the strong Arc before awaiting so stuck RPCs
                        // cannot re-form the pin cycle and block Inner Drop.
                        let (connection, session) = match inner.borrow.as_ref() {
                            Some(b) => (b.connection().clone(), inner.session.clone()),
                            None => continue,
                        };
                        drop(inner);
                        match list_remote_tools(connection.as_ref(), &session).await {
                            Ok(tools) => {
                                if let Some(inner) = weak.upgrade() {
                                    inner.remote_tools.store(Arc::new(tools));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "tool discovery: refresh after ToolsChanged failed"
                                );
                            }
                        }
                    }
                    crate::notification::HubNotification::ToolServerStatusChanged {
                        session_id,
                        status,
                    } if status.status == ToolServerLifecycleStatus::Disconnected => {
                        // Sync path — Arc is released at end of match arm.
                        inner.fail_inflight_calls_on_disconnect(&session_id);
                    }
                    _ => {}
                }
            }
        });
        *self.inner.discovery_handle.lock() = Some(handle);

        // Teardown may have won between subscribe and handle install: abort
        // the handle we just published (finish_teardown would have missed it).
        if self.inner.borrow.as_ref().is_some_and(|b| b.is_torn_down()) {
            if let Some(h) = self.inner.discovery_handle.lock().take() {
                h.abort();
            }
            if let (Some(borrow), Some(weak)) = (self.inner.borrow.as_ref(), inbox_weak.as_ref()) {
                let _ = borrow
                    .connection()
                    .demux()
                    .unregister_session_inbox_if_weak(&self.inner.session, weak);
            }
            *self.inner.session_inbox_tx.lock() = None;
        }
    }

    /// Cooperatively tear down this harness's connection borrow.
    ///
    /// Shared with both Drop paths via an at-most-once CAS inside
    /// `finish_teardown`. Aborts tool discovery, cancels the borrow token,
    /// untracks the session, and identity-unregisters this harness's demux
    /// inbox when last borrower. Idempotent across clones.
    ///
    /// **In-flight `call(...)` futures are NOT cancelled** by
    /// `shutdown`. The harness owns no run-loop — the underlying
    /// connection actor keeps reading inbound frames and each
    /// per-call [`ToolStream`] resolves naturally on its terminal
    /// frame. To force-drain in-flight calls, call
    /// `connection().request_shutdown()` (which closes the connection
    /// and surfaces every parked waiter as `NetworkError`) or drop
    /// the per-call stream.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        self.inner.finish_teardown();
        Ok(())
    }
}

/// Build the `ToolNotificationFrame` carrying a session-level event.
///
/// Both `tool_id` and `tool_call_id` are intentionally `None`: session
/// events are not associated with any single tool dispatch. If
/// `serde_json::to_value` were to fail (it cannot — every `SessionEvent`
/// field is a primitive), an empty `null` payload is sent rather than
/// panicking.
fn build_session_event_frame(event: &SessionEvent) -> ToolNotificationFrame {
    ToolNotificationFrame {
        tool_call_id: None,
        tool_id: None,
        notification: WireToolNotification::Custom(WireCustomNotification {
            kind: "session_event".to_owned(),
            payload: serde_json::to_value(event).unwrap_or_default(),
        }),
    }
}

/// Classify an inbound `Request` frame as a reverse-direction permission-request
/// hook, returning the decoded [`HookFrame`](echo_agent_tool_protocol::HookFrame) or `None`.
fn parse_permission_request_hook(value: &Value) -> Option<echo_agent_tool_protocol::HookFrame> {
    let method = value.get("method").and_then(Value::as_str)?;
    if method != Method::Hook.as_wire_str() {
        return None;
    }
    let hook = <echo_agent_tool_protocol::HookFrame as serde::Deserialize>::deserialize(
        value.get("params")?,
    )
    .ok()?;
    // Only request/response hooks (those with a reply leg) qualify.
    hook.hook_id.as_ref()?;
    match &hook.event {
        echo_agent_tool_protocol::HookEvent::Custom { kind, .. }
            if kind == PERMISSION_REQUEST_KIND =>
        {
            Some(hook)
        }
        _ => None,
    }
}

/// Dispatch one inbound `Request`: hand a permission-request hook to `handler`;
/// drop anything else (and permission requests with no handler registered).
fn dispatch_inbound_hook_request(
    value: &Value,
    handler: &parking_lot::Mutex<Option<HookRequestHandler>>,
) {
    let Some(hook) = parse_permission_request_hook(value) else {
        tracing::debug!("inbound request frame is not a permission-request hook; dropping");
        return;
    };
    // Clone out of the lock so the handler never runs while it is held.
    let handler = handler.lock().clone();
    match handler {
        // Isolate the host handler: an un-caught panic here would unwind the
        // shared inbox task and silently stop ALL notification delivery for the
        // session. Catch it, log, and keep the loop alive.
        Some(handler) => {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(hook))).is_err() {
                tracing::error!(
                    "inbound hook request handler panicked; dropping frame and continuing"
                );
            }
        }
        None => {
            tracing::debug!("inbound hook request received but no handler is registered; dropping")
        }
    }
}

/// Build the `hook_reply` notification answering a reverse-direction hook,
/// correlated to the request by
/// [`HookReplyFrame::hook_id`](echo_agent_tool_protocol::HookReplyFrame::hook_id).
fn build_hook_reply_notification(
    session_id: &SessionId,
    mut reply: echo_agent_tool_protocol::HookReplyFrame,
) -> JsonRpcNotification<echo_agent_tool_protocol::HookReplyFrame> {
    // Pin the frame's session id to the harness's bound session so params can
    // never disagree with the envelope; callers supply only hook_id + result.
    reply.session_id = session_id.clone();
    JsonRpcNotification {
        jsonrpc: JsonRpcVersion,
        session_id: Some(session_id.clone()),
        seq: None,
        method: Method::HookReply.as_wire_str().to_owned(),
        params: reply,
    }
}

/// Owned per-call state needed to build and dispatch the matching
/// `ToolCallCompleted` event. Lives inside an `Option` on
/// [`ObservedToolStream`] so the IDs can be moved into the event by
/// value (no string clones) on emission, and so a present `Some` is the
/// only "still owes a Completed" signal we need.
struct EmissionState {
    harness: ToolHarness,
    tool_call_id: ToolCallId,
    tool_id: ToolId,
    start: std::time::Instant,
}

/// Observability wrapper around a [`ToolStream`] returned by
/// [`ToolHarness::call`].
///
/// Emits exactly one [`SessionEvent::ToolCallCompleted`] for the call:
///
/// - `Success` — terminal `Ok` flowed through.
/// - `Error`   — terminal `Err` flowed through.
/// - `Cancelled` — stream was dropped before any terminal item, i.e.
///   the consumer (typically a `tokio::select!` on a cancel token) gave
///   up mid-dispatch.
///
/// Emission is fire-and-forget via `tokio::spawn` because both
/// `poll_next` and `Drop` are synchronous. The matching
/// `ToolCallStarted` was emitted by `call` before the stream was built.
struct ObservedToolStream {
    inner: ToolStream<echo_agent_tool_runtime::TypedToolOutput>,
    /// `Some` until the `ToolCallCompleted` event is scheduled, then
    /// `None` so neither a subsequent `poll_next` nor `Drop` double-emits.
    emission: Option<EmissionState>,
}

impl ObservedToolStream {
    fn new(
        inner: ToolStream<echo_agent_tool_runtime::TypedToolOutput>,
        harness: ToolHarness,
        tool_call_id: ToolCallId,
        tool_id: ToolId,
    ) -> Self {
        Self {
            inner,
            emission: Some(EmissionState {
                harness,
                tool_call_id,
                tool_id,
                start: std::time::Instant::now(),
            }),
        }
    }

    /// Spawn a fire-and-forget task that emits `ToolCallCompleted` with
    /// the given outcome, consuming the stashed [`EmissionState`].
    /// Best-effort no-op when no Tokio runtime is current — this only
    /// happens during process teardown, when missing observability
    /// events are an acceptable trade-off for a clean shutdown.
    fn spawn_completed(&mut self, outcome: ToolCallOutcome) {
        let Some(state) = self.emission.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return; // `state` drops here — emission lost on shutdown.
        };
        let event = SessionEvent::ToolCallCompleted {
            tool_call_id: state.tool_call_id.into_inner(),
            tool_name: state.tool_id.into_inner(),
            duration_ms: state.start.elapsed().as_millis() as u64,
            outcome,
        };
        let harness = state.harness;
        handle.spawn(async move {
            harness.emit_session_event(event).await;
        });
    }
}

impl Stream for ObservedToolStream {
    type Item = ToolStreamItem<echo_agent_tool_runtime::TypedToolOutput>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(Some(ToolStreamItem::Terminal(result))) = &poll
            && self.emission.is_some()
        {
            let outcome = if result.is_ok() {
                ToolCallOutcome::Success
            } else {
                ToolCallOutcome::Error
            };
            self.spawn_completed(outcome);
        }
        poll
    }
}

impl Drop for ObservedToolStream {
    fn drop(&mut self) {
        if self.emission.is_some() {
            self.spawn_completed(ToolCallOutcome::Cancelled);
        }
    }
}

impl Drop for ToolHarness {
    fn drop(&mut self) {
        // Best-effort fast path: skip while other ToolHarness clones still
        // exist (e.g. ObservedToolStream's internal clone during `call`).
        // Correctness does not depend on this gate — `ToolHarnessInner::Drop`
        // runs the same cleanup at true refcount-zero if this path skips.
        if Arc::strong_count(&self.inner) > 1 {
            return;
        }
        self.inner.finish_teardown();
    }
}

/// Send a `tool.call` JSON-RPC request, register a progress waiter,
/// and return a stream that interleaves progress notifications with
/// the eventual response terminal.
///
/// The progress waiter is registered BEFORE the request is sent so
/// any progress notification the server forwards while the request is
/// in flight lands in the per-call channel rather than being
/// dropped as `RouteOutcome::UnknownProgress`.
async fn dispatch_remote(
    connection: &Arc<HubConnection>,
    session_id: &SessionId,
    tool_id: ToolId,
    args: Value,
    ctx: ToolCallContext,
    trace_context: Option<String>,
) -> ToolStream<TypedToolOutput> {
    let call_id = ctx.call_id.clone();
    let cwd = ctx
        .extensions
        .get::<Cwd>()
        .map(|c| c.0.to_string_lossy().into_owned());
    let behavior_version = ctx.extensions.get::<BehaviorVersion>().map(|v| v.0.clone());
    // Clone the cancel-on-drop identifiers ONLY when opted in — the
    // default path pays no extra clone. `tool_id` itself moves into the
    // request params below.
    let cancel_on_drop = ctx
        .extensions
        .get::<CancelOnDrop>()
        .is_some_and(|c| c.0)
        .then(|| (session_id.clone(), tool_id.clone()));

    let (progress_tx, progress_rx) = mpsc::channel::<ToolCallProgressFrame>(PROGRESS_BUFFER);
    let demux = connection.demux();
    // Reject a concurrent caller passing the same call_id synchronously
    // instead of silently overwriting the live waiter, which would strand
    // the prior call's progress stream.
    if demux
        .try_register_progress_waiter(call_id.clone(), progress_tx)
        .is_err()
    {
        crate::metrics::call_id_collision();
        return terminal_only(Err(client_error_to_tool_error(ClientError::CallIdInUse {
            call_id,
        })));
    }

    let params = ToolCallParams {
        tool_call_id: call_id.clone(),
        tool_id,
        arguments: args,
        deadline_ms: None,
        behavior_version,
        cwd,
        trace_context,
    };
    // Serialize params to a Value first so the wire shape and THIS step's
    // `request_encoding` subcode stay unchanged. The envelope `to_string`
    // in `build_request_frame` can't fail for a valid `Value`, so its
    // differing error subcode is unreachable.
    let params_value = match serde_json::to_value(&params) {
        Ok(v) => v,
        Err(err) => {
            demux.unregister_progress_waiter(&call_id);
            return terminal_only(Err(ToolError::custom("request_encoding", err.to_string())));
        }
    };
    let (request_id, request_text) =
        match build_request_frame(connection, session_id, Method::ToolCall, params_value) {
            Ok(frame) => frame,
            Err(err) => {
                demux.unregister_progress_waiter(&call_id);
                return terminal_only(Err(client_error_to_tool_error(err)));
            }
        };

    let (response_tx, response_rx) = oneshot::channel();
    // Register with the session index so the in-flight short-circuit can fail
    // this call on a workspace Disconnected notification for the session.
    demux.register_call_response_waiter(request_id.clone(), session_id.clone(), response_tx);

    if let Err(err) = connection.send_outbound(request_text).await {
        // The demux still holds the parked waiters; pull them out so
        // the response oneshot doesn't sit forever on a request that
        // never reached the wire.
        demux.unregister_progress_waiter(&call_id);
        let _ = demux.take_response_waiter(&request_id);
        return terminal_only(Err(client_error_to_tool_error(err)));
    }

    Box::pin(RemoteCallStream::new(
        connection.clone(),
        params.tool_id,
        call_id,
        request_id,
        progress_rx,
        Box::pin(response_rx),
        cancel_on_drop,
    ))
}

/// Assemble a JSON-RPC request frame: allocate a request id, wrap
/// `params` under `method`, and serialize to text. The id is returned
/// for callers that park a response waiter; fire-and-forget callers
/// discard it. The enqueue (async [`HubConnection::send_outbound`] vs
/// sync [`HubConnection::try_send_outbound`]) stays with the caller.
fn build_request_frame<P: serde::Serialize>(
    connection: &HubConnection,
    session_id: &SessionId,
    method: Method,
    params: P,
) -> Result<(RequestId, String), ClientError> {
    let request_id = connection.try_alloc_request_id()?;
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: Some(session_id.clone()),
        method: method.as_wire_str().to_owned(),
        params,
    };
    let text = serde_json::to_string(&req).map_err(ClientError::from)?;
    Ok((request_id, text))
}

/// Unified stream that interleaves per-call progress with the
/// eventual JSON-RPC response and ends after exactly one terminal.
///
/// The progress receiver is polled while the response is pending;
/// once the response resolves the stream emits the matching
/// terminal item and returns `None` thereafter. The `Drop` impl
/// unregisters BOTH the per-call progress waiter (keyed by
/// `tool_call_id`) AND the response waiter (keyed by
/// `request_id`) from the demux — a stream that is dropped
/// before the response lands MUST NOT leak either map entry.
///
/// `cancel_on_drop` is `Some((session_id, tool_id))` only when the caller
/// opted in (the default path stores no extra clone); on `Drop` before a
/// terminal is polled it emits one best-effort call-scoped `Cancel` hook
/// so the workspace hard-cancels the in-flight call.
struct RemoteCallStream {
    connection: Arc<HubConnection>,
    /// Consumed exactly once when the terminal is built.
    tool_id: Option<ToolId>,
    call_id: ToolCallId,
    request_id: RequestId,
    progress_rx: Option<mpsc::Receiver<ToolCallProgressFrame>>,
    response_rx: Option<
        BoxFuture<'static, Result<Result<JsonRpcResponse, ClientError>, oneshot::error::RecvError>>,
    >,
    done: bool,
    cancel_on_drop: Option<(SessionId, ToolId)>,
}

impl RemoteCallStream {
    fn new(
        connection: Arc<HubConnection>,
        tool_id: ToolId,
        call_id: ToolCallId,
        request_id: RequestId,
        progress_rx: mpsc::Receiver<ToolCallProgressFrame>,
        response_rx: BoxFuture<
            'static,
            Result<Result<JsonRpcResponse, ClientError>, oneshot::error::RecvError>,
        >,
        cancel_on_drop: Option<(SessionId, ToolId)>,
    ) -> Self {
        Self {
            connection,
            tool_id: Some(tool_id),
            call_id,
            request_id,
            progress_rx: Some(progress_rx),
            response_rx: Some(response_rx),
            done: false,
            cancel_on_drop,
        }
    }

    /// Best-effort, non-blocking call-scoped `Cancel` hook for the
    /// cancel-on-drop path. `Drop` cannot `.await`, so the frame is
    /// try-enqueued onto the outbound channel; a full or closed channel
    /// drops it (the connection is already winding down / abandoning the
    /// call), matching the heartbeat-pong drop discipline.
    fn try_emit_cancel_on_drop(&self, session_id: &SessionId, tool_id: &ToolId) {
        let hook = echo_agent_tool_protocol::HookFrame::cancel(
            session_id.clone(),
            tool_id.clone(),
            self.call_id.clone(),
        );
        // Counts the attempt (including a frame later dropped on a full /
        // closed channel), matching `send_hook`'s count-before-send.
        crate::metrics::hook_send("cancel");
        let Ok((_request_id, text)) =
            build_request_frame(&self.connection, session_id, Method::Hook, hook)
        else {
            return;
        };
        if self.connection.try_send_outbound(text).is_err() {
            tracing::debug!(
                call_id = %self.call_id,
                "cancel-on-drop hook dropped (outbound channel full or closed)"
            );
        }
    }
}

impl Drop for RemoteCallStream {
    fn drop(&mut self) {
        let demux = self.connection.demux();
        // Pull both waiters out of the demux. Either may already be
        // gone (the response waiter is consumed by `route_response`
        // when the terminal frame lands; the progress waiter is
        // already removed by `unregister_progress_waiter` if that
        // ran). The `Option`-returning APIs make this idempotent.
        demux.unregister_progress_waiter(&self.call_id);
        let _ = demux.take_response_waiter(&self.request_id);

        // `cancel_on_drop` is `Some` only when opted in. `done` flips when
        // the consumer POLLS a terminal, so a drop after an unpolled
        // terminal still emits one cancel — benign, the server tombstones
        // the finished call (mirrors `ObservedToolStream`'s window).
        if !self.done
            && let Some((session_id, tool_id)) = self.cancel_on_drop.as_ref()
        {
            self.try_emit_cancel_on_drop(session_id, tool_id);
        }
    }
}

impl Stream for RemoteCallStream {
    type Item = ToolStreamItem<TypedToolOutput>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if self.done {
            return Poll::Ready(None);
        }

        // Poll progress first so any frames that have already been
        // routed by the demux drain in arrival order. The runtime's
        // stream invariant is `Progress* Terminal`; checking the
        // progress channel before the response future is what makes
        // that invariant hold even when the server has already shipped
        // the terminal frame by the time the consumer first polls.
        if let Some(rx) = self.progress_rx.as_mut() {
            match rx.poll_recv(cx) {
                Poll::Ready(Some(frame)) => {
                    return Poll::Ready(Some(ToolStreamItem::Progress(progress_from_frame(frame))));
                }
                Poll::Ready(None) | Poll::Pending => {}
            }
        }

        // Progress is empty (or closed); now check the response
        // future. A `Ready` outcome here is the terminal item.
        if let Some(fut) = self.response_rx.as_mut() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(Ok(resp))) => {
                    self.done = true;
                    self.response_rx = None;
                    self.progress_rx = None;
                    let terminal = match resp.outcome {
                        ResponseOutcome::Result(value) => match self.tool_id.take() {
                            Some(tool_id) => decode_call_result(tool_id, value),
                            None => return Poll::Ready(None),
                        },
                        ResponseOutcome::Error(err) => Err(error_from_envelope(err)),
                    };
                    return Poll::Ready(Some(ToolStreamItem::Terminal(terminal)));
                }
                Poll::Ready(Ok(Err(err))) => {
                    self.done = true;
                    self.response_rx = None;
                    self.progress_rx = None;
                    return Poll::Ready(Some(ToolStreamItem::Terminal(Err(
                        client_error_to_tool_error(err),
                    ))));
                }
                Poll::Ready(Err(_)) => {
                    self.done = true;
                    self.response_rx = None;
                    self.progress_rx = None;
                    return Poll::Ready(Some(ToolStreamItem::Terminal(Err(
                        ToolError::network_error("response waiter dropped (connection closed)"),
                    ))));
                }
                Poll::Pending => {}
            }
        } else {
            self.done = true;
            return Poll::Ready(None);
        }

        Poll::Pending
    }
}

// The `tool_call_result` success-body decode (`decode_call_result`),
// the progress-frame mapping (`progress_from_frame`), and the JSON-RPC
// envelope / `ToolErrorWire` error projections are all canonical in
// `echo_agent_computer_hub_core::remote`. Routing the harness through the same
// functions as the core remote proxy keeps both wire-decoding paths
// identical, so a future variant addition lands in one place.

/// Project a [`ClientError`] into a runtime [`ToolError`]. SDK
/// transport / protocol failures collapse to
/// [`ToolError::NetworkError`]; structurally-typed wire errors
/// pass through to [`tool_error_from_wire`]; the rest land on
/// [`ToolError::Custom`] keyed by a stable subcode.
fn client_error_to_tool_error(err: ClientError) -> ToolError {
    match err {
        ClientError::NetworkError(message) => ToolError::network_error(message),
        ClientError::ProtocolError(message) => ToolError::custom("protocol_error", message),
        ClientError::AuthError(message) => ToolError::permission_denied(message),
        ClientError::HandshakeAuthFailed { status } => {
            ToolError::permission_denied(format!("handshake auth failed (HTTP {status})"))
        }
        ClientError::RegistrationConflict(message) => {
            ToolError::custom("registration_conflict", message)
        }
        ClientError::BackpressureError(message) => ToolError::custom("backpressure", message),
        ClientError::Serde(message) => ToolError::custom("serde", message),
        ClientError::InvalidConfig(message) => ToolError::custom("invalid_config", message),
        ClientError::Wire(wire) => tool_error_from_wire(wire),
        ClientError::Closed(message) => ToolError::network_error(message),
        ClientError::InsecureScheme { url } => ToolError::custom(
            "insecure_scheme",
            format!("refusing plaintext ws:// to non-loopback host {url}"),
        ),
        err @ ClientError::CallIdInUse { .. } => {
            ToolError::custom("call_id_in_use", err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tool_types::ToolDescription;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug)]
    struct EchoTool {
        id: ToolId,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct EchoArgs {
        msg: String,
    }

    #[derive(Debug, Serialize)]
    struct EchoOut {
        echoed: String,
    }
    impl echo_agent_tool_runtime::ToolOutput for EchoOut {}

    impl Tool for EchoTool {
        type Args = EchoArgs;
        type Output = EchoOut;
        fn id(&self) -> ToolId {
            self.id.clone()
        }
        fn description(&self, _ctx: &ListToolsContext) -> ToolDescription {
            ToolDescription::new(self.id.as_str(), "echo")
        }
        async fn run(
            &self,
            _ctx: ToolCallContext,
            args: Self::Args,
        ) -> Result<Self::Output, ToolError> {
            Ok(EchoOut { echoed: args.msg })
        }
    }

    #[tokio::test]
    async fn request_turn_hook_errors_without_hub_connection() {
        use echo_agent_tool_protocol::turn_hook::{
            AfterTurnPayload, TurnHookOutcome, TurnHookRequest,
        };

        let harness = ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("test-session").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        );
        let req = TurnHookRequest::After(AfterTurnPayload {
            turn_number: 1,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 1,
            tool_call_count: 0,
            model_id: "echo-agent-3".to_string(),
            written_repo_paths: Vec::new(),
            cancellation_category: None,
            cancellation_context: None,
        });
        assert!(harness.request_turn_hook(&req).await.is_err());
    }

    fn pending_bind_test_harness() -> ToolHarness {
        ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("pending-bind-test").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        )
    }

    #[tokio::test]
    async fn no_pending_bind_awaits_to_self_and_probes_none() {
        let harness = pending_bind_test_harness();
        assert!(!harness.has_pending_bind());
        assert!(harness.try_bound().is_none());
        assert!(harness.await_bound().await.is_ok());
    }

    #[tokio::test]
    async fn pending_bind_resolves_to_connected_harness() {
        let harness = ToolHarness::local_with_pending_bind(
            LocalRegistry::new(),
            SessionId::new("twin").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async move { Ok(pending_bind_test_harness()) },
        );
        assert!(harness.has_pending_bind());
        let bound = harness.await_bound().await.expect("bind ok");
        assert!(!bound.has_pending_bind());
    }

    #[tokio::test]
    async fn pending_bind_failure_surfaces_error() {
        let harness = ToolHarness::local_with_pending_bind(
            LocalRegistry::new(),
            SessionId::new("twin").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async { Err(Arc::<str>::from("boom")) },
        );
        let err = harness.await_bound().await.expect_err("bind failed");
        assert!(err.contains("boom"));
    }

    #[tokio::test]
    async fn try_bound_is_none_until_resolved_then_some() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let harness = ToolHarness::local_with_pending_bind(
            LocalRegistry::new(),
            SessionId::new("twin").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async move {
                let _ = rx.await;
                Ok(pending_bind_test_harness())
            },
        );
        assert!(harness.try_bound().is_none(), "still binding");
        tx.send(()).unwrap();
        harness.await_bound().await.expect("bind ok");
        assert!(harness.try_bound().is_some(), "resolved");
    }

    #[tokio::test]
    async fn try_bound_flips_to_some_without_await() {
        let harness = ToolHarness::local_with_pending_bind(
            LocalRegistry::new(),
            SessionId::new("twin").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async move { Ok(pending_bind_test_harness()) },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match harness.try_bound() {
                Some(result) => {
                    result.expect("bind ok");
                    break;
                }
                None if std::time::Instant::now() < deadline => {
                    tokio::task::yield_now().await;
                }
                None => panic!("try_bound never observed the completed background bind"),
            }
        }
    }

    #[tokio::test]
    async fn lazy_bind_does_not_start_until_await_bound() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_in_fut = Arc::clone(&started);
        let harness = ToolHarness::local_with_lazy_bind(
            LocalRegistry::new(),
            SessionId::new("lazy").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async move {
                started_in_fut.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(pending_bind_test_harness())
            },
        );
        assert!(harness.has_pending_bind());

        // Probing must NOT kick off the bind, even after giving the runtime
        // several chances to make progress.
        for _ in 0..16 {
            assert!(harness.try_bound().is_none(), "lazy bind not started yet");
            tokio::task::yield_now().await;
        }
        assert!(
            !started.load(std::sync::atomic::Ordering::SeqCst),
            "lazy bind future must not run until await_bound"
        );

        // First await_bound starts and resolves it.
        let bound = harness.await_bound().await.expect("bind ok");
        assert!(!bound.has_pending_bind());
        assert!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            "await_bound must start the lazy bind"
        );
        assert!(harness.try_bound().is_some(), "resolved after await_bound");
    }

    #[tokio::test]
    async fn lazy_bind_failure_surfaces_error() {
        let harness = ToolHarness::local_with_lazy_bind(
            LocalRegistry::new(),
            SessionId::new("lazy").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
            async { Err(Arc::<str>::from("boom")) },
        );
        let err = harness.await_bound().await.expect_err("bind failed");
        assert!(err.contains("boom"));
    }

    #[test]
    fn local_registry_starts_empty() {
        let registry = LocalRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        let id = ToolId::new("missing").expect("valid");
        assert!(!registry.contains(&id));
        assert!(registry.find(&id).is_none());
        assert!(!registry.unregister(&id));
    }

    #[test]
    fn has_remote_tool_consults_only_the_remote_cache() {
        let registry = LocalRegistry::new();
        registry.register(EchoTool {
            id: ToolId::new("local_echo").expect("valid"),
        });
        let harness = ToolHarness::local_only_with(
            registry,
            SessionId::new("remote-cache-session").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        );

        assert!(!harness.has_remote_tool("bash"));
        assert!(!harness.has_remote_tool("local_echo"));

        harness.seed_remote_tools_for_tests(vec![
            ToolDescription::new("bash", "run a shell command"),
            ToolDescription::new("write_file", "write a file"),
        ]);
        assert!(harness.has_remote_tool("bash"));
        assert!(harness.has_remote_tool("write_file"));
        assert!(!harness.has_remote_tool("read_file"));
        assert!(!harness.has_remote_tool("local_echo"));

        // Cleared on unbind.
        harness.seed_remote_tools_for_tests(Vec::new());
        assert!(!harness.has_remote_tool("bash"));
    }

    #[test]
    fn local_registry_register_then_find_returns_handle() {
        let registry = LocalRegistry::new();
        let id = ToolId::new("echo").expect("valid");
        let prev = registry.register(EchoTool { id: id.clone() });
        assert!(prev.is_none(), "first register has nothing to displace");
        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&id));
        let handle = registry.find(&id).expect("registered tool resolves");
        assert_eq!(handle.id(), id);
    }

    #[test]
    fn local_registry_register_returns_displaced_handle_on_duplicate_id() {
        let registry = LocalRegistry::new();
        let id = ToolId::new("echo").expect("valid");
        let first = registry.register(EchoTool { id: id.clone() });
        assert!(first.is_none());
        let displaced = registry.register(EchoTool { id: id.clone() });
        assert!(
            displaced.is_some(),
            "second register on same id must return the displaced handle"
        );
        assert_eq!(registry.len(), 1, "id remains unique");
    }

    #[test]
    fn local_registry_unregister_returns_true_then_false() {
        let registry = LocalRegistry::new();
        let id = ToolId::new("echo").expect("valid");
        registry.register(EchoTool { id: id.clone() });
        assert!(
            registry.unregister(&id),
            "first unregister removes the entry"
        );
        assert!(
            !registry.unregister(&id),
            "second unregister sees the entry already gone"
        );
        assert!(registry.is_empty());
    }

    #[test]
    fn local_registry_register_arc_uses_shared_allocation() {
        let registry = LocalRegistry::new();
        let id = ToolId::new("echo_arc").expect("valid");
        let tool = Arc::new(EchoTool { id: id.clone() });
        let prev = registry.register_arc(tool.clone());
        assert!(prev.is_none());
        // The original Arc is still held by the test (refcount ≥ 2):
        // the registry stores its own clone via `ErasedTool::from_arc`,
        // so dropping the test's clone does not invalidate the registry.
        drop(tool);
        let handle = registry.find(&id).expect("handle still present");
        assert_eq!(handle.id(), id);
    }

    #[test]
    fn local_registry_register_alias_resolves_both_ids() {
        let registry = LocalRegistry::new();
        let full_id = ToolId::new("slack___search_channels").expect("valid");
        let bare_id = ToolId::new("search_channels").expect("valid");

        registry.register(EchoTool {
            id: full_id.clone(),
        });
        assert!(registry.contains(&full_id));
        assert!(!registry.contains(&bare_id));

        assert!(registry.register_alias(bare_id.clone(), &full_id));
        assert!(registry.contains(&bare_id));

        let h1 = registry.find(&full_id).expect("full id resolves");
        let h2 = registry.find(&bare_id).expect("bare alias resolves");
        assert_eq!(h1.id(), h2.id(), "both resolve to the same tool");
    }

    #[test]
    fn local_registry_register_alias_returns_false_for_missing_target() {
        let registry = LocalRegistry::new();
        let alias = ToolId::new("alias").expect("valid");
        let missing = ToolId::new("missing").expect("valid");
        assert!(!registry.register_alias(alias.clone(), &missing));
        assert!(!registry.contains(&alias));
    }

    #[test]
    fn local_registry_list_tools_preserves_insertion_order() {
        let registry = LocalRegistry::new();
        let names = [
            "web_search",
            "code_execution",
            "generate_image",
            "browse_page",
        ];
        for name in &names {
            registry.register(EchoTool {
                id: ToolId::new(*name).expect("valid"),
            });
        }
        let ctx = ListToolsContext::new();
        let listed: Vec<String> = registry
            .list_tools(&ctx)
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            listed,
            names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "list_tools must return tools in registration (insertion) order"
        );
    }

    #[tokio::test]
    async fn builder_missing_pool_errors() {
        let url = Url::parse("ws://127.0.0.1:0/v1/tools").expect("valid url");
        let cred = AuthCredential::bearer("ignored");
        let session = SessionId::new("s").expect("valid");
        let err = ToolHarnessBuilder::default()
            .url(url)
            .auth(cred)
            .session(session)
            .build()
            .await
            .expect_err("missing pool must fail");
        assert!(matches!(err, ClientError::InvalidConfig(msg) if msg.contains("missing pool")));
    }

    #[tokio::test]
    async fn builder_missing_url_errors() {
        let pool = HubConnectionPool::new();
        let cred = AuthCredential::bearer("ignored");
        let session = SessionId::new("s").expect("valid");
        let err = ToolHarnessBuilder::default()
            .pool(pool)
            .auth(cred)
            .session(session)
            .build()
            .await
            .expect_err("missing url must fail");
        assert!(matches!(err, ClientError::InvalidConfig(msg) if msg.contains("missing url")));
    }

    #[tokio::test]
    async fn builder_missing_auth_errors() {
        let pool = HubConnectionPool::new();
        let url = Url::parse("ws://127.0.0.1:0/v1/tools").expect("valid url");
        let session = SessionId::new("s").expect("valid");
        let err = ToolHarnessBuilder::default()
            .pool(pool)
            .url(url)
            .session(session)
            .build()
            .await
            .expect_err("missing auth must fail");
        assert!(matches!(err, ClientError::InvalidConfig(msg) if msg.contains("missing auth")));
    }

    #[tokio::test]
    async fn builder_missing_session_errors() {
        let pool = HubConnectionPool::new();
        let url = Url::parse("ws://127.0.0.1:0/v1/tools").expect("valid url");
        let cred = AuthCredential::bearer("ignored");
        let err = ToolHarnessBuilder::default()
            .pool(pool)
            .url(url)
            .auth(cred)
            .build()
            .await
            .expect_err("missing session must fail");
        assert!(matches!(err, ClientError::InvalidConfig(msg) if msg.contains("missing session")));
    }

    #[test]
    fn local_registry_clone_shares_backing_store() {
        let a = LocalRegistry::new();
        let id = ToolId::new("shared").expect("valid");
        a.register(EchoTool { id: id.clone() });

        let b = a.clone();
        assert_eq!(b.len(), 1);
        assert!(b.find(&id).is_some());

        // Register through b, visible through a
        let id2 = ToolId::new("shared2").expect("valid");
        b.register(EchoTool { id: id2.clone() });
        assert_eq!(a.len(), 2);
        assert!(a.find(&id2).is_some());
    }

    #[test]
    fn local_registry_unregister_visible_to_clones() {
        let a = LocalRegistry::new();
        let id = ToolId::new("ephemeral").expect("valid");
        a.register(EchoTool { id: id.clone() });

        let b = a.clone();
        assert!(b.unregister(&id));
        assert!(a.is_empty());
    }

    // ── Session event frame construction ────────────────────────────

    #[test]
    fn session_event_frame_has_no_tool_correlation_ids() {
        let event = SessionEvent::TurnStarted {
            turn_number: 1,
            model_id: "echo-agent-3".into(),
            yolo_mode: false,
        };
        let frame = build_session_event_frame(&event);
        assert!(frame.tool_id.is_none());
        assert!(frame.tool_call_id.is_none());
        match &frame.notification {
            WireToolNotification::Custom(c) => {
                assert_eq!(c.kind, "session_event");
                assert_eq!(c.payload["event_type"], "turn_started");
                assert_eq!(c.payload["turn_number"], 1);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn session_event_frame_round_trips_through_serde() {
        let event = SessionEvent::ToolCallStarted {
            tool_call_id: "call-42".into(),
            tool_name: "read_file".into(),
            turn_number: 3,
        };
        let frame = build_session_event_frame(&event);
        let json = serde_json::to_value(&frame).unwrap();
        let back: ToolNotificationFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    // ── Local-only call() must not wrap with observability ─────────

    fn build_local_only_harness_with(tool: EchoTool) -> ToolHarness {
        let session = SessionId::new("test-observed").expect("valid");
        let registry = LocalRegistry::new();
        registry.register(tool);
        ToolHarness::local_only_with(registry, session, Default::default())
    }

    #[tokio::test]
    async fn local_only_call_returns_raw_stream_no_observability_wrap() {
        // Sanity: a local-only harness has `borrow == None`, so `call()`
        // must skip the `ObservedToolStream` wrap and emit nothing — the
        // existing local-tool test surface stays byte-for-byte identical.
        let tool_id = ToolId::new("echo").expect("valid");
        let harness = build_local_only_harness_with(EchoTool {
            id: tool_id.clone(),
        });
        let mut stream = harness
            .call(
                tool_id,
                serde_json::json!({ "msg": "hi" }),
                ToolCallContext::default(),
            )
            .await;

        let mut saw_terminal_ok = false;
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            if let ToolStreamItem::Terminal(Ok(_)) = item {
                saw_terminal_ok = true;
            }
        }
        assert!(saw_terminal_ok, "local echo must yield Terminal(Ok)");
    }

    #[tokio::test]
    async fn local_only_emit_session_event_is_noop() {
        // No server borrow → `emit_session_event` must short-circuit before
        // building a frame. The test passes if the call returns without
        // touching `send_notification` (no panic from a missing
        // connection actor).
        let harness = ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("test-no-hub").expect("valid"),
            Default::default(),
        );
        harness
            .emit_session_event(SessionEvent::PhaseChanged {
                phase: echo_agent_tool_protocol::session_event::SessionPhase::Idle,
            })
            .await;
    }

    /// Wrap a [`HookFrame`](echo_agent_tool_protocol::HookFrame) in the JSON-RPC `Request` envelope.
    fn inbound_hook_request_frame(hook: &echo_agent_tool_protocol::HookFrame) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "h1",
            "session_id": hook.session_id.as_str(),
            "method": Method::Hook.as_wire_str(),
            "params": serde_json::to_value(hook).expect("serialize hook"),
        })
    }

    #[test]
    fn parse_permission_request_hook_matches_request_response_hook() {
        let hook = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-7".to_owned(),
            PERMISSION_REQUEST_KIND.to_owned(),
            serde_json::json!({ "tool_call_id": "call-1" }),
        );
        let parsed = parse_permission_request_hook(&inbound_hook_request_frame(&hook))
            .expect("permission request matches");
        assert_eq!(parsed.hook_id.as_deref(), Some("hook-7"));
        match parsed.event {
            echo_agent_tool_protocol::HookEvent::Custom { kind, payload } => {
                assert_eq!(kind, PERMISSION_REQUEST_KIND);
                assert_eq!(payload, serde_json::json!({ "tool_call_id": "call-1" }));
            }
            other => panic!("expected Custom event, got {other:?}"),
        }
    }

    #[test]
    fn parse_permission_request_hook_rejects_other_custom_kind() {
        let hook = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-7".to_owned(),
            echo_agent_tool_protocol::turn_hook::TURN_HOOK_KIND.to_owned(),
            serde_json::json!({}),
        );
        assert!(parse_permission_request_hook(&inbound_hook_request_frame(&hook)).is_none());
    }

    #[test]
    fn parse_permission_request_hook_rejects_missing_hook_id() {
        let hook = echo_agent_tool_protocol::HookFrame::custom(
            SessionId::new("s1").expect("valid"),
            PERMISSION_REQUEST_KIND.to_owned(),
            serde_json::json!({}),
        );
        assert!(hook.hook_id.is_none());
        assert!(parse_permission_request_hook(&inbound_hook_request_frame(&hook)).is_none());
    }

    #[test]
    fn parse_permission_request_hook_rejects_non_custom_event() {
        let hook = echo_agent_tool_protocol::HookFrame {
            session_id: SessionId::new("s1").expect("valid"),
            tool_id: None,
            call_id: None,
            hook_id: Some("hook-7".to_owned()),
            event: echo_agent_tool_protocol::HookEvent::Pause,
            trace_context: None,
        };
        assert!(parse_permission_request_hook(&inbound_hook_request_frame(&hook)).is_none());
    }

    #[test]
    fn parse_permission_request_hook_rejects_non_hook_method() {
        let hook = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-7".to_owned(),
            PERMISSION_REQUEST_KIND.to_owned(),
            serde_json::json!({}),
        );
        let mut frame = inbound_hook_request_frame(&hook);
        frame["method"] = serde_json::json!("tool_call_request");
        assert!(parse_permission_request_hook(&frame).is_none());
    }

    #[tokio::test]
    async fn registered_handler_receives_matching_request_only() {
        let harness = ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("s1").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        );
        let (tx, mut rx) = mpsc::channel::<echo_agent_tool_protocol::HookFrame>(4);
        harness.set_hook_request_handler(move |hook| {
            tx.try_send(hook).expect("handler channel has capacity");
        });
        let slot = &harness.inner.hook_request_handler;

        let other = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-0".to_owned(),
            echo_agent_tool_protocol::turn_hook::TURN_HOOK_KIND.to_owned(),
            serde_json::json!({}),
        );
        dispatch_inbound_hook_request(&inbound_hook_request_frame(&other), slot);
        assert!(
            rx.try_recv().is_err(),
            "non-permission request must be dropped"
        );

        let perm = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-7".to_owned(),
            PERMISSION_REQUEST_KIND.to_owned(),
            serde_json::json!({ "tool_call_id": "call-1" }),
        );
        dispatch_inbound_hook_request(&inbound_hook_request_frame(&perm), slot);
        let received = rx.recv().await.expect("handler invoked");
        assert_eq!(received.hook_id.as_deref(), Some("hook-7"));
    }

    #[test]
    fn dispatch_inbound_hook_request_without_handler_invokes_nothing() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let slot: parking_lot::Mutex<Option<HookRequestHandler>> =
            parking_lot::Mutex::new(Some(Arc::new(move |_hook| {
                counter.fetch_add(1, Ordering::SeqCst);
            })));
        let perm = echo_agent_tool_protocol::HookFrame::custom_request(
            SessionId::new("s1").expect("valid"),
            "hook-7".to_owned(),
            PERMISSION_REQUEST_KIND.to_owned(),
            serde_json::json!({}),
        );
        let frame = inbound_hook_request_frame(&perm);

        dispatch_inbound_hook_request(&frame, &slot);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        *slot.lock() = None;
        dispatch_inbound_hook_request(&frame, &slot);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no-handler path must invoke nothing"
        );
    }

    #[test]
    fn hook_reply_notification_has_correct_wire_shape() {
        let session = SessionId::new("s1").expect("valid");
        let reply = echo_agent_tool_protocol::HookReplyFrame {
            session_id: session.clone(),
            hook_id: "hook-7".to_owned(),
            result: serde_json::json!({ "outcome": "approve" }),
        };
        let notif = build_hook_reply_notification(&session, reply);
        assert_eq!(notif.method, Method::HookReply.as_wire_str());
        let json: Value = serde_json::from_str(&serde_json::to_string(&notif).unwrap()).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["method"], "hook_reply");
        assert_eq!(json["session_id"], "s1");
        assert!(
            json.get("id").is_none(),
            "hook_reply is a notification — must carry no id"
        );
        assert!(json.get("seq").is_none(), "None seq must be omitted");
        assert_eq!(json["params"]["hook_id"], "hook-7");
        assert_eq!(json["params"]["session_id"], "s1");
        assert_eq!(json["params"]["result"]["outcome"], "approve");
    }

    #[tokio::test]
    async fn send_hook_reply_errors_without_hub_connection() {
        let harness = ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("test-session").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        );
        let reply = echo_agent_tool_protocol::HookReplyFrame {
            session_id: harness.session().clone(),
            hook_id: "hook-7".to_owned(),
            result: Value::Null,
        };
        assert!(harness.send_hook_reply(reply).await.is_err());
    }

    #[test]
    fn try_send_hook_reply_errors_without_hub_connection() {
        let harness = ToolHarness::local_only_with(
            LocalRegistry::new(),
            SessionId::new("test-session").expect("valid session"),
            echo_agent_tool_runtime::TypedExtensions::default(),
        );
        let reply = echo_agent_tool_protocol::HookReplyFrame {
            session_id: harness.session().clone(),
            hook_id: "hook-7".to_owned(),
            result: Value::Null,
        };
        assert!(harness.try_send_hook_reply(reply).is_err());
    }

    // --- discovery / teardown lifecycle (connection-leak regression) ---

    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::Router;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::{Message, WebSocket};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use serde_json::json;
    use tokio::net::TcpListener;

    use crate::auth::AuthCredential;
    use crate::pool::HubConnectionPool;

    async fn spawn_discovery_mock_hub() -> SocketAddr {
        let app = Router::new().route("/v1/tools", get(discovery_ws_upgrade));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        tokio::task::yield_now().await;
        addr
    }

    async fn discovery_ws_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(discovery_handle_socket)
    }

    async fn discovery_handle_socket(mut socket: WebSocket) {
        let _ = socket.recv().await;
        let ack = json!({
            "connection_id": "discovery-mock",
            "user_id": "test",
            "computer_hub_version": "test",
            "supported_protocol_versions": ["1.0.0"],
        });
        let _ = socket.send(Message::Text(ack.to_string().into())).await;
        // Ignore WS Ping/Pong/Close: keepalive fires immediately after hello.
        while let Some(Ok(msg)) = socket.recv().await {
            let Message::Text(text) = msg else { continue };
            let Ok(value) = serde_json::from_str::<Value>(text.as_ref()) else {
                continue;
            };
            let method = value.get("method").and_then(Value::as_str).unwrap_or("");
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            match method {
                "session_open" => {
                    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": {} });
                    let _ = socket.send(Message::Text(resp.to_string().into())).await;
                }
                "tools.list" => {
                    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } });
                    let _ = socket.send(Message::Text(resp.to_string().into())).await;
                }
                _ => {}
            }
        }
    }

    async fn build_connected_harness(
        session: &str,
    ) -> (ToolHarness, Arc<HubConnectionPool>, Arc<HubConnection>) {
        let addr = spawn_discovery_mock_hub().await;
        let url = Url::parse(&format!("ws://{addr}/v1/tools")).expect("valid url");
        let pool = HubConnectionPool::new();
        let harness = ToolHarnessBuilder::default()
            .pool(pool.clone())
            .url(url)
            .auth(AuthCredential::bearer("ignored"))
            .session(SessionId::new(session).expect("valid"))
            .build()
            .await
            .expect("build harness");
        let conn = harness.connection().expect("connected").clone();
        (harness, pool, conn)
    }

    async fn poll_until(mut pred: impl FnMut() -> bool, label: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if pred() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for: {label}");
    }

    #[tokio::test]
    async fn discovery_task_exits_when_inbox_closes_after_drop() {
        let (harness, _pool, conn) = build_connected_harness("weak-exit-eof").await;
        harness.start_tool_discovery().await;
        assert!(harness.discovery_task_started_for_tests());

        // Steal the JoinHandle before Drop aborts it so we can observe exit.
        let handle = harness
            .inner
            .discovery_handle
            .lock()
            .take()
            .expect("discovery handle installed");

        drop(harness);
        poll_until(
            || conn.bound_session_count() == 0,
            "session untracked after harness drop",
        )
        .await;

        // Last-borrower unregister closes the demux inbox → event rx EOF.
        let join = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            join.is_ok(),
            "discovery task must complete once harness strong refs are gone"
        );
    }

    #[tokio::test]
    async fn discovery_task_exits_on_weak_upgrade_failure() {
        let (harness, _pool, conn) = build_connected_harness("weak-upgrade-exit").await;
        let session = harness.session().clone();
        harness.start_tool_discovery().await;
        assert!(harness.discovery_task_started_for_tests());

        // Steal handle so Drop's abort cannot complete the task for us.
        let handle = harness
            .inner
            .discovery_handle
            .lock()
            .take()
            .expect("discovery handle installed");

        // Extra session track so Drop is not last → does not unregister inbox.
        // Discovery keeps waiting on a live rx with only a dead Weak.
        conn.track_session(session.clone());
        drop(harness);
        assert_eq!(
            conn.bound_session_count(),
            1,
            "peer track keeps the session binding (and demux inbox) alive"
        );

        // Force the upgrade-failure branch (not EOF): deliver a notification
        // while no strong ToolHarnessInner remains.
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": session.as_str(),
            "method": "tools_changed",
            "params": {
                "session_id": session.as_str(),
                "added": [],
                "removed": [],
            }
        });
        let outcome = conn.demux().route(frame);
        assert!(
            matches!(outcome, crate::demux::RouteOutcome::Session),
            "notification must reach the still-registered session inbox, got {outcome:?}"
        );

        let join = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            join.is_ok(),
            "discovery task must exit via weak.upgrade() == None on a post-drop notification"
        );

        let _ = conn.untrack_session(&session);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inner_drop_teardown_runs_under_racing_clones() {
        let (harness, pool, conn) = build_connected_harness("race-drop").await;
        harness.start_tool_discovery().await;

        // Peer track: a double-untrack regression would zero this out.
        let peer_session = SessionId::new("race-drop-peer").expect("valid");
        conn.track_session(peer_session.clone());
        assert_eq!(conn.bound_session_count(), 2);

        let n = 16;
        let barrier = Arc::new(tokio::sync::Barrier::new(n));
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let clone = harness.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                drop(clone);
            }));
        }
        drop(harness);
        for h in handles {
            h.await.expect("join dropper");
        }

        poll_until(
            || conn.bound_session_count() == 1,
            "harness session untracked; peer track remains",
        )
        .await;
        assert_eq!(
            conn.untrack_session(&peer_session),
            Some(0),
            "peer track must still be exactly 1 after racing drops (no double-untrack)"
        );

        let weak = Arc::downgrade(&conn);
        drop(conn);
        poll_until(
            || pool.sweep_idle(Duration::ZERO) == 1 || weak.upgrade().is_none(),
            "connection becomes pool-evictable",
        )
        .await;
        // Either sweep already took it, or a second sweep is a no-op once gone.
        let _ = pool.sweep_idle(Duration::ZERO);
        assert!(
            weak.upgrade().is_none(),
            "connection must be fully released after racing clone drops"
        );
    }

    #[tokio::test]
    async fn transient_inner_upgrade_does_not_skip_teardown() {
        let (harness, pool, conn) = build_connected_harness("transient-upgrade").await;
        harness.start_tool_discovery().await;

        // Hold a transient strong Arc of the inner (simulates discovery
        // upgrade mid-notification) while dropping every ToolHarness.
        let transient = harness.inner.clone();
        drop(harness);

        // Wrapper Drop sees strong_count > 1 and skips; cleanup must still
        // run when the transient ref drops (ToolHarnessInner::Drop).
        assert_eq!(
            conn.bound_session_count(),
            1,
            "session still tracked while transient inner Arc is held"
        );
        drop(transient);

        poll_until(
            || conn.bound_session_count() == 0,
            "session untracked after transient inner drop",
        )
        .await;

        let weak = Arc::downgrade(&conn);
        drop(conn);
        poll_until(
            || {
                let _ = pool.sweep_idle(Duration::ZERO);
                weak.upgrade().is_none()
            },
            "connection released after transient upgrade race",
        )
        .await;
    }

    #[tokio::test]
    async fn same_session_rebind_replaces_prior_inbox() {
        let addr = spawn_discovery_mock_hub().await;
        let url = Url::parse(&format!("ws://{addr}/v1/tools")).expect("valid url");
        let pool = HubConnectionPool::new();
        let session = SessionId::new("rebind-session").expect("valid");
        let cred = AuthCredential::bearer("ignored");

        let first = ToolHarnessBuilder::default()
            .pool(pool.clone())
            .url(url.clone())
            .auth(cred.clone())
            .session(session.clone())
            .build()
            .await
            .expect("first harness");
        first.start_tool_discovery().await;
        let first_handle = first
            .inner
            .discovery_handle
            .lock()
            .take()
            .expect("first discovery handle");

        let second = ToolHarnessBuilder::default()
            .pool(pool.clone())
            .url(url)
            .auth(cred)
            .session(session)
            .build()
            .await
            .expect("second harness");
        second.start_tool_discovery().await;
        assert!(second.discovery_task_started_for_tests());

        // register_session_inbox replaces the prior sender → first rx EOFs.
        let join = tokio::time::timeout(Duration::from_secs(5), first_handle).await;
        assert!(
            join.is_ok(),
            "first discovery task must exit when second harness rebinds the inbox"
        );

        // Last-borrower gate: dropping first must not unregister second's inbox.
        let conn = second.connection().expect("connected").clone();
        let second_session = second.session().clone();
        drop(first);
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": second_session.as_str(),
            "method": "tools_changed",
            "params": {
                "session_id": second_session.as_str(),
                "added": [],
                "removed": [],
            }
        });
        let outcome = conn.demux().route(frame);
        assert!(
            matches!(outcome, crate::demux::RouteOutcome::Session),
            "second's demux inbox must remain after first drop, got {outcome:?}"
        );
        assert!(
            !second
                .inner
                .discovery_handle
                .lock()
                .as_ref()
                .expect("second discovery handle still installed")
                .is_finished(),
            "second discovery must survive first harness drop"
        );

        drop(second);
        poll_until(
            || conn.bound_session_count() == 0,
            "session fully released after last harness drop",
        )
        .await;
    }

    #[tokio::test]
    async fn shutdown_unregisters_session_inbox() {
        let (harness, pool, conn) = build_connected_harness("shutdown-inbox").await;
        let session = harness.session().clone();
        harness.start_tool_discovery().await;
        assert_eq!(conn.bound_session_count(), 1);

        harness.shutdown().await.expect("shutdown");
        assert_eq!(conn.bound_session_count(), 0);

        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": session.as_str(),
            "method": "tools_changed",
            "params": {
                "session_id": session.as_str(),
                "added": [],
                "removed": [],
            }
        });
        let outcome = conn.demux().route(frame);
        assert!(
            !matches!(outcome, crate::demux::RouteOutcome::Session),
            "shutdown must unregister the demux inbox, got {outcome:?}"
        );

        let weak = Arc::downgrade(&conn);
        drop(harness);
        drop(conn);
        assert_eq!(pool.sweep_idle(Duration::ZERO), 1);
        assert!(weak.upgrade().is_none());
    }
}
