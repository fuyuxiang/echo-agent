//! Tool-server runtime: builder, handler trait, and inbound dispatch loop.
//!
//! A [`ToolServer`] is the SDK-side counterpart to a server-side
//! `ToolServer` connection. The builder collects:
//!
//! - the [`crate::HubConnectionPool`] to attach to,
//! - the server URL,
//! - an [`crate::AuthCredential`],
//! - one or more [`ToolServerHandler`] implementations,
//! - zero or more sessions (bound during [`ToolServer::run`]).
//!
//! On [`ToolServerBuilder::build`] the server registers its identity
//! and tools with the server. [`ToolServer::run`] drives the inbound loop:
//! every server-issued `tool_call_request` is decoded, dispatched to
//! the matching handler, and the response is shipped back over the
//! shared connection. [`ToolServer::shutdown`] cooperatively stops
//! `run` and unregisters everything; [`Drop`] is a best-effort
//! fallback that schedules the same cleanup on a background task.

use async_trait::async_trait;
use dashmap::DashMap;
use echo_agent_tool_protocol::{
    ConnectionKind, HookEvent, HookFrame, HookReplyFrame, JsonRpcError, JsonRpcId,
    JsonRpcNotification, JsonRpcResponse, JsonRpcVersion, Method, ResponseOutcome, SessionId,
    ToolCallId, ToolCallParams, ToolCallProgressFrame, ToolCallResult, ToolErrorWire, ToolId,
    ToolOutputWire, ToolServerEvictParams, error_codes,
};
use echo_agent_tool_runtime::{
    BehaviorVersion, Cancellation, Cwd, ToolCallContext, ToolError, ToolProgress, ToolStream,
    ToolStreamItem, TraceContext, TypedToolOutput,
};
use echo_agent_tool_types::ToolDescription;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;

use crate::auth::{AuthCredential, AuthProvider};
use crate::cancel::CancelRegistry;
use crate::connection::{
    ConnectCallback, ConnectionTuning, DisconnectCallback, HubConnection, ReconnectCallback,
    ReconnectEvent, TerminalCloseCallback,
};
use crate::connection_borrow::ConnectionBorrow;
use crate::demux::InboundFrame;
use crate::error::ClientError;
use crate::pool::HubConnectionPool;

/// Fired after reconnect `serve` replay completes (async settle).
pub type ReconnectSettledCallback = Box<dyn Fn() + Send + Sync + 'static>;

/// Outcome of a `system.notify` request. `Accepted` is the server's ack to forward;
/// `ForwardingUnsupported` is an older server that lacks the method (`-32601`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemNotifyAck {
    Accepted,
    ForwardingUnsupported,
}

/// Serialized JSON byte length without allocating the string.
fn json_serialized_len(value: &Value) -> Result<usize, ClientError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|e| ClientError::Serde(e.to_string()))?;
    Ok(counter.0)
}

fn system_notify_ack_from_outcome(
    outcome: ResponseOutcome<Value>,
) -> Result<SystemNotifyAck, ClientError> {
    match outcome {
        ResponseOutcome::Result(_) => Ok(SystemNotifyAck::Accepted),
        // Plain `-32601` (no `data` discriminator) means the server lacks the method;
        // require `data` absent so a richer error still flows through the normal taxonomy.
        ResponseOutcome::Error(err)
            if err.data.is_none()
                && echo_agent_tool_protocol::error_codes::string_for(err.code)
                    == Some("method_not_found") =>
        {
            Ok(SystemNotifyAck::ForwardingUnsupported)
        }
        ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
    }
}

/// Per-session inbound queue depth. The spawn-per-request dispatcher
/// dequeues immediately, so the inbox barely lags and the
/// inbox-full path is now a rare relief valve rather than the steady
/// state; 64 is kept as a comfortable burst buffer ahead of the
/// admission-deadline backpressure.
const SESSION_INBOX_BUFFER: usize = 64;

type SessionHandlerMap =
    Arc<parking_lot::RwLock<HashMap<SessionId, Vec<Arc<dyn ToolServerHandler>>>>>;

/// A resolved session binding: handlers plus the bind-report fields echoed
/// in the `session.bind` response.
#[derive(Default)]
pub struct ResolvedSessionHandlers {
    pub handlers: Vec<Arc<dyn ToolServerHandler>>,
    /// Tool ids the resolver declined to serve; forwarded as
    /// [`echo_agent_tool_protocol::SessionBindResult::unserved_tool_ids`].
    pub unserved_tool_ids: Vec<String>,
    /// Human-readable reason the resolver failed the toolset closed;
    /// forwarded as [`echo_agent_tool_protocol::SessionBindResult::resolve_error`].
    pub resolve_error: Option<String>,
}

impl ResolvedSessionHandlers {
    /// A fully-served binding with no divergence to report.
    pub fn full(handlers: Vec<Arc<dyn ToolServerHandler>>) -> Self {
        Self {
            handlers,
            unserved_tool_ids: Vec::new(),
            resolve_error: None,
        }
    }
}

/// Resolves a session's served handler set from the raw `session.bind`
/// params. When unset, sessions bind with a clone of `initial_handlers`.
///
/// Returning `Err` **fails the bind**: the server receives an error response
/// (and classifies it as bind-unavailable so the harness can re-provision)
/// instead of a "successful" bind that advertises zero model-facing tools —
/// which would make every subsequent tool call fail as route-missing with no
/// hint of the real cause.
pub type SessionHandlerResolver = Arc<
    dyn Fn(
            SessionId,
            Option<serde_json::Value>,
        )
            -> futures::future::BoxFuture<'static, Result<ResolvedSessionHandlers, ToolError>>
        + Send
        + Sync,
>;

/// User-facing tool implementation.
///
/// The server speaks JSON, so handlers receive `serde_json::Value`
/// arguments and return a `ToolStream<Value>`. Implementations that
/// already use [`echo_agent_tool_runtime::Tool`] can adapt by calling the
/// underlying tool's `execute` and serialising the typed output.
#[async_trait]
pub trait ToolServerHandler: Send + Sync + 'static {
    /// Stable identity used by the server to route to this handler.
    fn tool_id(&self) -> ToolId;

    /// Model-facing description and argument schema.
    fn description(&self) -> ToolDescription;

    /// Argument schema. Default `None` — many handlers don't expose a
    /// schema independent of the description.
    fn input_schema(&self) -> Option<Value> {
        None
    }

    /// Execute one tool call.
    ///
    /// Implementations MUST honour the [`ToolStream`] invariant: zero
    /// or more `Progress` items followed by exactly one `Terminal`.
    ///
    /// `Progress` items are forwarded as `tool_call_progress`
    /// notifications; the `Terminal` item is shipped as the response.
    async fn handle_call(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput>;

    /// Receive a harness-issued hook for `session_id`.
    ///
    /// `frame.tool_id` was set when the harness routed the hook to a
    /// specific tool (e.g. [`HookEvent::Cancel`] with the matching
    /// `tool_id`); `None` when the hook is session-wide (broadcast).
    /// Implementations route by `frame.event` shape and may
    /// abort in-flight calls correlated by `frame.call_id`.
    ///
    /// Default is a no-op so existing handlers do not need to opt in.
    /// Override to receive cancel / pause / resume / session-ended /
    /// custom hooks.
    #[allow(unused_variables)]
    async fn handle_hook(&self, session_id: SessionId, frame: HookFrame) {}

    /// Answer a request/response hook (one whose `hook_id` is set); first `Some` wins, `None` declines.
    #[allow(unused_variables)]
    async fn handle_hook_request(&self, session_id: SessionId, frame: HookFrame) -> Option<Value> {
        None
    }

    /// Handle a server-issued `tool_server.evict` (graceful-shutdown request),
    /// fanned out to the evicted session's handlers. Implementations should
    /// drain in-flight work within `params.grace_period_ms`, after which the
    /// server force-closes the connection. Default is a no-op.
    #[allow(unused_variables)]
    async fn handle_evict(&self, params: ToolServerEvictParams) {}
}

/// Builder for [`ToolServer`]. See module docs for end-to-end usage.
#[derive(Default)]
pub struct ToolServerBuilder {
    pool: Option<Arc<HubConnectionPool>>,
    url: Option<Url>,
    auth: Option<Arc<dyn AuthProvider>>,
    sessions: Vec<SessionId>,
    handlers: Vec<Arc<dyn ToolServerHandler>>,
    on_reconnect: Option<Arc<ReconnectCallback>>,
    /// Fired after reconnect serve replay finishes (async settle, not the sync
    /// socket-up `on_reconnect`). Use for readiness markers that must not
    /// precede server session re-serve.
    on_reconnect_settled: Option<Arc<ReconnectSettledCallback>>,
    on_disconnect: Option<Arc<DisconnectCallback>>,
    on_terminal_close: Option<Arc<TerminalCloseCallback>>,
    on_connect: Option<Arc<ConnectCallback>>,
    metadata: Option<serde_json::Value>,
    server_id: Option<echo_agent_tool_protocol::ServerId>,
    server_description: Option<String>,
    alpha_test_key: Option<String>,
    allow_insecure_ws: bool,
    session_max_inflight: Option<usize>,
    conn_max_inflight: Option<usize>,
    global_max_inflight: Option<usize>,
    admission_wait_timeout: Option<std::time::Duration>,
    ws_ping_interval: Option<std::time::Duration>,
    ws_liveness_deadline: Option<std::time::Duration>,
    reconnect_backoff: Option<Arc<[std::time::Duration]>>,
    reconnect_after_terminal_close_codes: Vec<u16>,
    initial_connect_attempt_timeout: Option<std::time::Duration>,
    session_handler_resolver: Option<SessionHandlerResolver>,
    binary_version: Option<String>,
    image_capabilities: Vec<String>,
}

impl ToolServerBuilder {
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

    /// Max concurrent *running* calls per session (default 16),
    /// enforced by the spawned per-request dispatcher.
    pub fn session_max_inflight(mut self, max: usize) -> Self {
        self.session_max_inflight = Some(max);
        self
    }

    /// Max concurrent *running* calls across all sessions on this
    /// connection (default 256).
    pub fn conn_max_inflight(mut self, max: usize) -> Self {
        self.conn_max_inflight = Some(max);
        self
    }

    /// Process-wide concurrent running-call ceiling (default 1024). Shared
    /// by every connection via a once-initialized semaphore; the
    /// `ECHO_AGENT_TOOL_SERVER_GLOBAL_MAX_INFLIGHT` env var overrides this at
    /// startup. Because the global cell initializes once, the first server
    /// built in the process fixes the process-wide value.
    pub fn global_max_inflight(mut self, max: usize) -> Self {
        self.global_max_inflight = Some(max);
        self
    }

    /// Bounded wait before an admission attempt is rejected with the
    /// overloaded (-32016 "tool_busy") error (default 3s). A single
    /// deadline spans all three semaphore acquisitions.
    pub fn admission_wait_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.admission_wait_timeout = Some(timeout);
        self
    }

    /// Override the WebSocket keepalive ping cadence on a freshly-opened
    /// connection (default 30s). Not setting it preserves the default.
    pub fn with_ws_ping_interval(mut self, interval: std::time::Duration) -> Self {
        self.ws_ping_interval = Some(interval);
        self
    }

    /// Override the inbound-liveness deadline on a freshly-opened
    /// connection: if no RTT proof (WS/app pong) arrives within this
    /// window, the connection is declared dead and reconnected.
    /// Hub→client pings and one-way data do not re-arm.
    ///
    /// Default (also used for a zero value): `min(4× ping, 120s)` — 120s
    /// at the default 30s ping, still under the hub's ~150s idle. Explicit
    /// values are honored verbatim; keep them comfortably above the ping
    /// interval (a value at or below the ping interval churns healthy idle
    /// connections and is logged as a warning at connect).
    pub fn with_ws_liveness_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.ws_liveness_deadline = Some(deadline);
        self
    }

    /// Override the reconnect backoff schedule on a freshly-opened
    /// connection (default: the built-in exponential table capped at 10s).
    /// Each wait is `Uniform(0, min(last_slot, max(slot, 1s)))`, not the
    /// literal slot — a single-element `[25ms]` table is jittered in
    /// `[0, 25ms)`. An empty schedule falls back to the default.
    pub fn with_reconnect_backoff(mut self, schedule: Vec<std::time::Duration>) -> Self {
        self.reconnect_backoff = Some(schedule.into());
        self
    }

    /// Allowlist specific 4100–4199 terminal close codes to reconnect after.
    /// Empty (default) keeps the protocol contract: the actor stops on every
    /// terminal close. Only restorable-session codes (e.g.
    /// [`crate::connection::CLOSE_CODE_SANDBOX_TERMINATED`]) belong here.
    pub fn reconnect_after_terminal_close_codes(
        mut self,
        codes: impl IntoIterator<Item = u16>,
    ) -> Self {
        let mut codes: Vec<u16> = codes.into_iter().collect();
        codes.sort_unstable();
        codes.dedup();
        self.reconnect_after_terminal_close_codes = codes;
        self
    }

    /// Per-attempt budget for the initial connect (WebSocket upgrade +
    /// hello/hello_ack). Default (also used for a zero value): 10s. A peer
    /// that accepts the socket but never answers would otherwise hang the
    /// caller indefinitely; the SDK retries transient failures a bounded
    /// number of times with jittered backoff before surfacing the error.
    pub fn with_initial_connect_attempt_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.initial_connect_attempt_timeout = Some(timeout);
        self
    }

    /// Connection knobs handed to [`HubConnection::connect`].
    /// `reconnect_attempt_reset_after` is left `None` so the SDK applies
    /// the 10 s production dwell — not zero, not "never".
    pub(crate) fn connection_tuning(&self) -> ConnectionTuning {
        ConnectionTuning {
            ws_ping_interval: self.ws_ping_interval,
            ws_liveness_deadline: self.ws_liveness_deadline,
            reconnect_backoff: self.reconnect_backoff.clone(),
            reconnect_attempt_reset_after: None,
            reconnect_after_terminal_close_codes: self.reconnect_after_terminal_close_codes.clone(),
            initial_connect_attempt_timeout: self.initial_connect_attempt_timeout,
        }
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

    /// Bind `session_id` on the underlying connection. May be called
    /// repeatedly; each call adds one session that `run()` will bind
    /// via `bind_session_local`.
    pub fn session(mut self, session_id: SessionId) -> Self {
        self.sessions.push(session_id);
        self
    }

    /// Register a tool. May be called repeatedly.
    pub fn tool<H: ToolServerHandler>(mut self, handler: H) -> Self {
        self.handlers.push(Arc::new(handler));
        self
    }

    /// Register a dynamically-typed tool handler.
    pub fn tool_dyn(mut self, handler: Arc<dyn ToolServerHandler>) -> Self {
        self.handlers.push(handler);
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

    /// Optional callback fired after reconnect `serve` replay completes for all
    /// active sessions (runs on the reconnect task, after the sync
    /// [`Self::on_reconnect`] / hello). Prefer this for readiness markers so
    /// "server-ready" means registered **and** session tools re-served.
    ///
    /// It only fires when **every** session re-served successfully **and** no
    /// disconnect or terminal close raced the (async) replay; otherwise it is
    /// skipped and the next reconnect's replay settles instead. This keeps a
    /// readiness marker from being resurrected while the socket is already down
    /// again.
    pub fn on_reconnect_settled<F>(mut self, cb: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_reconnect_settled = Some(Arc::new(Box::new(cb) as ReconnectSettledCallback));
        self
    }

    /// Optional callback fired when the live server socket drops or closes.
    pub fn on_disconnect<F>(mut self, cb: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_disconnect = Some(Arc::new(Box::new(cb) as DisconnectCallback));
        self
    }

    /// Optional callback fired with the close code when the server sends a
    /// terminal close (4100–4199). Invoked before [`Self::on_disconnect`].
    /// Advances the same disconnect epoch as [`Self::on_disconnect`] so a
    /// reconnect settle that still holds the pre-close generation cannot fire
    /// [`Self::on_reconnect_settled`] after this callback. The actor still
    /// stops afterwards unless the code is allowlisted via
    /// [`Self::reconnect_after_terminal_close_codes`].
    pub fn on_terminal_close<F>(mut self, cb: F) -> Self
    where
        F: Fn(u16) + Send + Sync + 'static,
    {
        self.on_terminal_close = Some(Arc::new(Box::new(cb) as TerminalCloseCallback));
        self
    }

    /// Optional callback fired once on the initial successful connect, after
    /// the writer task enters its loop and before the reader actor starts.
    /// The first keepalive may still be in flight.
    pub fn on_connect<F>(mut self, cb: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_connect = Some(Arc::new(Box::new(cb) as ConnectCallback));
        self
    }

    /// Stable server identity for `servers.list` discovery and
    /// `session.open` addressing. Sent in the hello frame.
    pub fn server_id(mut self, id: echo_agent_tool_protocol::ServerId) -> Self {
        self.server_id = Some(id);
        self
    }

    /// Server description for `servers.list` discovery.
    pub fn server_description(mut self, desc: impl Into<String>) -> Self {
        self.server_description = Some(desc.into());
        self
    }

    /// Attach opaque metadata to every tool registered by this server.
    /// Propagated to `ServerInfo.metadata` in `servers.list` responses.
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Install a per-session handler resolver (the binding path).
    pub fn session_handler_resolver(mut self, resolver: SessionHandlerResolver) -> Self {
        self.session_handler_resolver = Some(resolver);
        self
    }

    /// Version of the embedding binary, echoed as
    /// [`echo_agent_tool_protocol::SessionBindResult::binary_version`].
    pub fn binary_version(mut self, version: impl Into<String>) -> Self {
        self.binary_version = Some(version.into());
        self
    }

    /// Image capability tokens echoed on every bind as
    /// [`echo_agent_tool_protocol::SessionBindResult::image_capabilities`]. The
    /// caller validates and sorts them; the SDK forwards them verbatim.
    pub fn image_capabilities(mut self, tokens: Vec<String>) -> Self {
        self.image_capabilities = tokens;
        self
    }

    /// Resolve the pool entry, bind sessions, register tools.
    ///
    /// Returns a [`ToolServer`] ready to be driven via
    /// [`ToolServer::run`]. On any mid-loop failure (a tool's
    /// `register_tool` returning `Err`, a session bind failing) the
    /// builder rolls back every successfully-registered tool and
    /// every successfully-bound session before returning the original
    /// error, so a failed `build()` does not leak server-side state.
    pub async fn build(self) -> Result<ToolServer, ClientError> {
        let tuning = self.connection_tuning();
        let pool = self
            .pool
            .ok_or_else(|| ClientError::InvalidConfig("missing pool".to_owned()))?;
        let url = self
            .url
            .ok_or_else(|| ClientError::InvalidConfig("missing url".to_owned()))?;
        let auth = self
            .auth
            .ok_or_else(|| ClientError::InvalidConfig("missing auth".to_owned()))?;
        if self.handlers.is_empty() {
            return Err(ClientError::InvalidConfig(
                "ToolServer must register at least one tool".to_owned(),
            ));
        }

        // Pre-create shared state so the on_reconnect callback can
        // signal `serve` replay after a reconnect.
        let active_sessions = parking_lot::Mutex::new(Vec::<SessionId>::new());
        let reconnect_notify = Arc::new(tokio::sync::Notify::new());

        // Compose the internal reconnect handler (signal serve replay)
        // with the user's optional callback.
        let user_on_reconnect = self.on_reconnect.clone();
        let notify_clone = Arc::clone(&reconnect_notify);
        let combined_reconnect: Arc<ReconnectCallback> =
            Arc::new(Box::new(move |event: ReconnectEvent| {
                notify_clone.notify_one();
                if let Some(ref cb) = user_on_reconnect {
                    cb(event);
                }
            }));

        // Compose the internal disconnect handler (bump the epoch so a
        // disconnect racing an in-flight serve replay is observed by the
        // reconnect task) with the user's optional callback.
        let disconnect_epoch = Arc::new(AtomicU64::new(0));
        let user_on_disconnect = self.on_disconnect.clone();
        let epoch_for_disconnect = Arc::clone(&disconnect_epoch);
        let combined_disconnect: Arc<DisconnectCallback> = Arc::new(Box::new(move || {
            epoch_for_disconnect.fetch_add(1, Ordering::Release);
            if let Some(ref cb) = user_on_disconnect {
                cb();
            }
        }));

        // Terminal close runs before on_disconnect. Bump the same epoch so a
        // reconnect settle that still holds the pre-close generation cannot
        // fire on_reconnect_settled after last_close_code is latched.
        let user_on_terminal_close = self.on_terminal_close;
        let epoch_for_terminal_close = Arc::clone(&disconnect_epoch);
        let combined_terminal_close: Arc<TerminalCloseCallback> = Arc::new(Box::new(move |code| {
            epoch_for_terminal_close.fetch_add(1, Ordering::Release);
            if let Some(ref cb) = user_on_terminal_close {
                cb(code);
            }
        }));

        let borrow = ConnectionBorrow::acquire(
            pool,
            url,
            auth,
            ConnectionKind::ToolServer,
            Some(combined_reconnect),
            Some(combined_disconnect),
            self.on_connect,
            Some(combined_terminal_close),
            self.server_id,
            self.server_description,
            self.metadata,
            self.alpha_test_key,
            self.allow_insecure_ws,
            tuning,
        )
        .await?;

        let global_sem = crate::admission::global_semaphore(
            self.global_max_inflight
                .unwrap_or(crate::admission::DEFAULT_GLOBAL_MAX_INFLIGHT),
        );
        let admission = Arc::new(crate::admission::Admission::new(
            self.session_max_inflight
                .unwrap_or(crate::admission::DEFAULT_SESSION_MAX_INFLIGHT),
            self.conn_max_inflight
                .unwrap_or(crate::admission::DEFAULT_CONN_MAX_INFLIGHT),
            global_sem,
            self.admission_wait_timeout
                .unwrap_or(crate::admission::DEFAULT_ADMISSION_WAIT_TIMEOUT),
        ));

        let inner = Arc::new(ToolServerInner {
            borrow,
            initial_handlers: self.handlers,
            initial_sessions: self.sessions,
            session_handler_resolver: self.session_handler_resolver,
            session_handlers: Arc::new(parking_lot::RwLock::new(HashMap::new())),
            session_unserved: parking_lot::RwLock::new(HashMap::new()),
            session_resolve_errors: parking_lot::RwLock::new(HashMap::new()),
            binary_version: self.binary_version,
            image_capabilities: self.image_capabilities,
            notification_fwd: Arc::new(parking_lot::Mutex::new(None)),
            parsed_notif_tx: Arc::new(parking_lot::Mutex::new(None)),
            session_handles: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            active_sessions,
            dynamic_tool_mu: tokio::sync::Mutex::new(()),
            session_bind_mu: tokio::sync::Mutex::new(()),
            reconnect_notify,
            on_reconnect_settled: self.on_reconnect_settled,
            disconnect_epoch,
            admission,
            cancels: Arc::new(DashMap::new()),
            donation_pumps: parking_lot::Mutex::new(DonationPumps::default()),
        });
        Ok(ToolServer { inner: Some(inner) })
    }
}

/// Running tool-server attached to a pooled [`HubConnection`].
///
/// `Clone` is an `Arc` bump. Prefer [`Self::shutdown`]; [`Drop`] tears down
/// only for the last strong owner. Use [`Self::downgrade`] for observers.
pub struct ToolServer {
    /// `Option` so [`Drop`] can take the `Arc` for [`Arc::into_inner`].
    inner: Option<Arc<ToolServerInner>>,
}

/// Non-owning handle to a [`ToolServer`]; [`Self::upgrade`] per use.
#[derive(Clone, Default)]
pub struct WeakToolServer {
    inner: std::sync::Weak<ToolServerInner>,
}

impl WeakToolServer {
    pub fn upgrade(&self) -> Option<ToolServer> {
        self.inner
            .upgrade()
            .map(|inner| ToolServer { inner: Some(inner) })
    }
}

struct ToolServerInner {
    borrow: ConnectionBorrow,
    /// Handlers passed to the builder — cloned into each new session.
    initial_handlers: Vec<Arc<dyn ToolServerHandler>>,
    /// Sessions passed to the builder. `run()` binds these via
    /// `bind_session_local` so tools are registered and inboxes are
    /// created before the dispatch loop starts.
    initial_sessions: Vec<SessionId>,
    session_handler_resolver: Option<SessionHandlerResolver>,
    /// Per-session handler maps. Each session owns its own handler vec.
    session_handlers: SessionHandlerMap,
    /// Per-session unserved tool ids from the last resolver run; same
    /// lifetime as the `session_handlers` entry.
    session_unserved: parking_lot::RwLock<HashMap<SessionId, Vec<String>>>,
    /// Per-session fail-closed resolve reason from the last resolver run;
    /// same lifetime as the `session_handlers` entry.
    session_resolve_errors: parking_lot::RwLock<HashMap<SessionId, String>>,
    binary_version: Option<String>,
    image_capabilities: Vec<String>,

    /// Raw notification forwarding channel. Session loops write here;
    /// the parsing task (spawned in `run()`) reads and parses into
    /// `HubNotification` events sent to `parsed_notif_tx`.
    notification_fwd: Arc<parking_lot::Mutex<Option<mpsc::Sender<Value>>>>,
    /// Parsed notification sender. Set by `subscribe_notifications`;
    /// the parsing task in `run()` bridges `notification_fwd` → this.
    parsed_notif_tx:
        Arc<parking_lot::Mutex<Option<mpsc::Sender<crate::notification::HubNotification>>>>,
    /// Session loops spawned by `bind_session_local`, keyed by session ID.
    session_handles: Arc<parking_lot::Mutex<HashMap<SessionId, tokio::task::JoinHandle<()>>>>,
    /// All sessions currently active on this server.
    active_sessions: parking_lot::Mutex<Vec<SessionId>>,
    /// Serializes `register_tool_dynamic` / `unregister_tool_dynamic`.
    /// `tokio::sync::Mutex` because the critical section spans `.await`.
    dynamic_tool_mu: tokio::sync::Mutex<()>,
    /// Serializes session binds against each other and against
    /// `unbind_session`, so the soft-rebind liveness decision and the
    /// destructive full-rebind setup are atomic (no check-then-act race
    /// between two concurrent binds, and no unbind sneaking between a
    /// bind's liveness check and its return). Binds/unbinds are rare
    /// lifecycle events; a global mutex is contention-free in practice.
    session_bind_mu: tokio::sync::Mutex<()>,
    /// Signalled by the on_reconnect callback so `run()` can replay
    /// `serve` for every active session after a reconnect.
    reconnect_notify: Arc<tokio::sync::Notify>,
    /// Optional readiness / settle hook after serve replay (see
    /// [`ToolServerBuilder::on_reconnect_settled`]).
    on_reconnect_settled: Option<Arc<ReconnectSettledCallback>>,
    /// Bumped on every disconnect and terminal close. The reconnect task
    /// snapshots this before `serve` replay and only fires `on_reconnect_settled`
    /// if it is unchanged afterward — so a disconnect or terminal close racing
    /// the (async) replay cannot resurrect a stale ready marker while the socket
    /// is already down.
    disconnect_epoch: Arc<AtomicU64>,
    /// Three-tier (session/connection/global) admission controller. Drives
    /// the bounded-wait-then-overloaded backpressure on the spawned path.
    admission: Arc<crate::admission::Admission>,
    /// Per-session strict-cancellation registries. Created in
    /// `bind_session_local` alongside the inbox + admission semaphore, and
    /// drained-and-cancelled on `unbind_session` / `shutdown` so detached
    /// `execute_call` tasks wind down promptly.
    cancels: Arc<DashMap<SessionId, Arc<CancelRegistry>>>,
    /// Trace/log/metric donation pump senders, fenced by
    /// [`ToolServer::flush_donations`] on unbind/shutdown.
    donation_pumps: parking_lot::Mutex<DonationPumps>,
}

/// The three symmetric donation pump senders. Each is fenced
/// independently by `flush_donations_inner` so a teardown never abandons
/// a queued batch.
#[derive(Default)]
struct DonationPumps {
    traces: Option<mpsc::Sender<crate::donate_pump::PumpMsg>>,
    logs: Option<mpsc::Sender<crate::donate_pump::PumpMsg>>,
    metrics: Option<mpsc::Sender<crate::donate_pump::PumpMsg>>,
}

impl Clone for ToolServer {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.as_ref().map(Arc::clone),
        }
    }
}

impl std::fmt::Debug for ToolServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner();
        f.debug_struct("ToolServer")
            .field("active_sessions", &*inner.active_sessions.lock())
            .field("handler_count", &inner.initial_handlers.len())
            .finish_non_exhaustive()
    }
}

impl ToolServer {
    fn inner(&self) -> &Arc<ToolServerInner> {
        self.inner
            .as_ref()
            .expect("ToolServer used after Drop took the Arc")
    }

    pub fn downgrade(&self) -> WeakToolServer {
        WeakToolServer {
            inner: Arc::downgrade(self.inner()),
        }
    }

    /// Underlying connection. Useful for tests that need to assert
    /// pool dedup.
    pub fn connection(&self) -> &Arc<HubConnection> {
        self.inner().borrow.connection()
    }

    /// Snapshot of all currently active sessions.
    pub fn active_sessions(&self) -> Vec<SessionId> {
        self.inner().active_sessions.lock().clone()
    }

    /// Handlers for a specific session.
    pub fn handlers_for_session(&self, session_id: &SessionId) -> Vec<Arc<dyn ToolServerHandler>> {
        self.inner()
            .session_handlers
            .read()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Unserved tool ids reported by the resolver for a session's last bind.
    pub fn unserved_for_session(&self, session_id: &SessionId) -> Vec<String> {
        self.inner()
            .session_unserved
            .read()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Fail-closed resolve reason reported by the resolver for a session's
    /// last bind, if any.
    pub fn resolve_error_for_session(&self, session_id: &SessionId) -> Option<String> {
        self.inner()
            .session_resolve_errors
            .read()
            .get(session_id)
            .cloned()
    }

    /// Bind a new session in two steps:
    ///
    /// 1. **Local setup** (`bind_session_local`): register the session
    ///    on the connection, create the demux inbox, and spawn the
    ///    per-session dispatch loop.
    /// 2. **Publish** (`serve`): send a `serve` frame to the server with
    ///    the full tool snapshot so harnesses see the tools immediately.
    pub async fn bind_session(&self, session_id: SessionId) -> Result<(), ClientError> {
        self.bind_session_local(session_id.clone()).await?;
        self.serve(session_id).await
    }

    /// Register a session on the connection, create the demux inbox,
    /// and spawn the per-session dispatch loop.
    ///
    /// After binding, the caller should send a `serve` frame to
    /// publish the tool snapshot.
    pub async fn bind_session_local(&self, session_id: SessionId) -> Result<(), ClientError> {
        self.bind_session_local_with_metadata(session_id, None)
            .await
    }

    /// [`bind_session_local`] with the raw `session.bind` params for an
    /// installed [`SessionHandlerResolver`].
    ///
    /// Non-destructive for a live session (**soft rebind**): when the
    /// session's dispatch loop is still running, a repeated bind only
    /// refreshes serve state (resolver re-run → `session_handlers` /
    /// `session_unserved`) and leaves the inbox, cancel registry, and
    /// dispatch loop untouched, so in-flight tool calls survive. The
    /// full destructive setup (new inbox + registry + loop, cancelling
    /// anything stale) only runs when the previous loop is dead or the
    /// session was never bound.
    ///
    /// Handler-set caveat: a soft rebind re-runs the resolver, so the
    /// resolver must tolerate re-execution while handler instances from a
    /// previous bind may still be mid-call; subsequent hook frames
    /// (pause/resume/cancel fan-out) target the refreshed handler set,
    /// while in-flight calls keep their pre-rebind handler clones and the
    /// shared cancel registry.
    pub async fn bind_session_local_with_metadata(
        &self,
        session_id: SessionId,
        bind_params: Option<serde_json::Value>,
    ) -> Result<(), ClientError> {
        // Serialized against other binds and `unbind_session` so the
        // liveness decision below cannot race a concurrent bind/teardown.
        let _bind_guard = self.inner().session_bind_mu.lock().await;
        let connection = self.inner().borrow.connection();
        let sid = session_id;

        // Resolve handlers BEFORE mutating any session state so a resolver
        // failure fails the bind cleanly (nothing tracked, no inbox, no
        // session loop) and a retry re-runs from scratch.
        let resolved = match &self.inner().session_handler_resolver {
            // Re-run on every bind — including a soft rebind of a live
            // session — so a retry after a failed bind recreates the
            // session (resolver-owned) and refreshes advertised tools.
            Some(resolver) => Some(
                resolver(sid.clone(), bind_params)
                    .await
                    .map_err(|err| ClientError::Wire(ToolErrorWire::from(err)))?,
            ),
            None => None,
        };

        connection.track_session(sid.clone());
        {
            let mut sessions = self.inner().active_sessions.lock();
            if !sessions.contains(&sid) {
                sessions.push(sid.clone());
            }
        }

        match resolved {
            Some(resolved) => {
                self.inner()
                    .session_handlers
                    .write()
                    .insert(sid.clone(), resolved.handlers);
                self.inner()
                    .session_unserved
                    .write()
                    .insert(sid.clone(), resolved.unserved_tool_ids);
                {
                    let mut errors = self.inner().session_resolve_errors.write();
                    match resolved.resolve_error {
                        Some(reason) => {
                            errors.insert(sid.clone(), reason);
                        }
                        None => {
                            errors.remove(&sid);
                        }
                    }
                }
            }
            None => {
                self.inner()
                    .session_handlers
                    .write()
                    .entry(sid.clone())
                    .or_insert_with(|| self.inner().initial_handlers.clone());
            }
        }

        // Soft rebind: a live dispatch loop means this is a redundant
        // `session.bind` for a healthy session — refresh serve state only
        // (done above; the server's bind response reads `handlers_for_session`).
        // Replacing the inbox/registry/loop would cancel every in-flight
        // tool call. `session_bind_mu` keeps a concurrent bind/unbind from
        // invalidating this check before we return; the registry check
        // additionally rejects a loop in its post-teardown exit tail
        // (`cancel_all` closes the registry before the JoinHandle finishes),
        // making the gate best-effort-safe against non-serialized teardown.
        // A dead loop falls through to the full (destructive) rebind, which
        // preserves stale-token cleanup.
        {
            let loop_alive = {
                let handles = self.inner().session_handles.lock();
                handles.get(&sid).is_some_and(|h| !h.is_finished())
            };
            let registry_open = self
                .inner()
                .cancels
                .get(&sid)
                .is_some_and(|r| !r.is_closed());
            if loop_alive && registry_open {
                crate::metrics::session_soft_rebind();
                tracing::debug!(
                    %sid,
                    "bind: session loop alive — soft rebind (serve state refreshed, \
                     in-flight calls preserved)"
                );
                return Ok(());
            }
        }

        let (tx, rx) = mpsc::channel(SESSION_INBOX_BUFFER);
        connection.demux().register_session_inbox(sid.clone(), tx);
        // Tie the per-session admission semaphore to the session-loop
        // lifetime (created here, removed on unbind / loop exit) so a
        // straggler call cannot recreate a leaked entry after teardown.
        self.inner().admission.ensure_session(&sid);
        // Per-session cancellation registry, same lifetime as the loop.
        // A full rebind (previous loop dead — the live-loop case returned
        // above) drains-and-cancels any stale registry before installing
        // a fresh one so tokens from a previous loop never linger.
        let cancels = Arc::new(CancelRegistry::default());
        if let Some(old) = self.inner().cancels.insert(sid.clone(), cancels.clone()) {
            old.cancel_all();
        }

        let sh = self.inner().session_handlers.clone();
        let notification_fwd = self.inner().notification_fwd.clone();
        let conn = connection.clone();
        let loop_sid = sid.clone();
        let admission = self.inner().admission.clone();
        let cancels_owner = self.inner().cancels.clone();
        let handle = tokio::spawn(async move {
            run_session_loop(
                loop_sid,
                rx,
                conn,
                sh,
                notification_fwd,
                admission,
                cancels,
                cancels_owner,
            )
            .await;
        });
        // Replace the previous (dead) loop's handle, if any. Abort is a
        // no-op for a finished handle; with binds serialized under
        // `session_bind_mu` no concurrent full rebind can have installed a
        // live handle in between, so this never kills live work.
        if let Some(old_handle) = self.inner().session_handles.lock().insert(sid, handle) {
            old_handle.abort();
        }

        Ok(())
    }

    /// Send a `serve` frame for a session, publishing the full tool
    /// snapshot. Idempotent — the server diffs and emits `tools_changed`.
    ///
    /// On reconnect, call `serve()` per active session to replay state.
    pub async fn serve(&self, session_id: SessionId) -> Result<(), ClientError> {
        // Build tool descriptions while holding the read lock so the
        // handler list cannot mutate between read and serialization.
        let tools: Vec<echo_agent_tool_protocol::ToolDescriptionWithSchema> = {
            let map = self.inner().session_handlers.read();
            let handlers = map.get(&session_id);
            handlers
                .map(|h| {
                    h.iter()
                        .map(|h| echo_agent_tool_protocol::ToolDescriptionWithSchema {
                            description: h.description(),
                            input_schema: h.input_schema(),
                            capabilities: None,
                            notification_schemas: None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let params = echo_agent_tool_protocol::ServeParams { tools };

        let connection = self.inner().borrow.connection();
        connection.serve(session_id, params).await?;
        Ok(())
    }

    /// Register a tool handler at runtime for the given sessions.
    ///
    /// Adds the handler to the local session handler map and replays
    /// `serve` for each affected session so the server sees the updated
    /// tool set.
    pub async fn register_tool_dynamic(
        &self,
        handler: Arc<dyn ToolServerHandler>,
        sessions: Vec<SessionId>,
    ) -> Result<(), ClientError> {
        let _guard = self.inner().dynamic_tool_mu.lock().await;

        let tool_id = handler.tool_id();

        // Reject duplicates within the target sessions.
        {
            let map = self.inner().session_handlers.read();
            for sid in &sessions {
                if let Some(handlers) = map.get(sid)
                    && handlers.iter().any(|h| h.tool_id() == tool_id)
                {
                    return Err(ClientError::InvalidConfig(format!(
                        "tool_id {tool_id} is already registered for session {sid}"
                    )));
                }
            }
        }

        // Insert into each target session's handler list.
        {
            let mut map = self.inner().session_handlers.write();
            for sid in &sessions {
                map.entry(sid.clone()).or_default().push(handler.clone());
            }
        }

        // Replay `serve` for each affected session so the server sees
        // the updated tool set.
        for sid in sessions {
            self.serve(sid).await?;
        }

        Ok(())
    }

    /// Remove a dynamically registered tool from a specific session.
    /// Returns `Ok(false)` if not found.
    pub async fn unregister_tool_dynamic(
        &self,
        tool_id: &ToolId,
        session_id: &SessionId,
    ) -> Result<bool, ClientError> {
        let _guard = self.inner().dynamic_tool_mu.lock().await;

        // Check existence in the target session.
        {
            let map = self.inner().session_handlers.read();
            let found = map
                .get(session_id)
                .is_some_and(|h| h.iter().any(|h| h.tool_id() == *tool_id));
            if !found {
                return Ok(false);
            }
        }

        // Remove from the session's handler list.
        {
            let mut map = self.inner().session_handlers.write();
            if let Some(handlers) = map.get_mut(session_id) {
                handlers.retain(|h| h.tool_id() != *tool_id);
            }
        }

        // Replay `serve` for the affected session so the server sees the
        // tool removal.
        self.serve(session_id.clone()).await?;

        Ok(true)
    }

    /// Unbind a session: tear down the session loop, remove handlers,
    /// and unregister the session.
    pub async fn unbind_session(&self, session_id: &SessionId) -> Result<(), ClientError> {
        // Serialized against binds (see `session_bind_mu`): an unbind must
        // not interleave with a bind's liveness check / setup.
        let _bind_guard = self.inner().session_bind_mu.lock().await;
        let connection = self.inner().borrow.connection();

        self.inner()
            .active_sessions
            .lock()
            .retain(|s| s != session_id);

        self.inner().session_handlers.write().remove(session_id);
        self.inner().session_unserved.write().remove(session_id);
        self.inner()
            .session_resolve_errors
            .write()
            .remove(session_id);

        // Teardown ordering: drain-and-cancel every in-flight
        // call's token FIRST so detached `execute_call` tasks wind down via
        // their `select!`, THEN abort the dispatcher and remove the inbox /
        // admission / registry entries.
        if let Some((_, registry)) = self.inner().cancels.remove(session_id) {
            registry.cancel_all();
        }

        if let Some(handle) = self.inner().session_handles.lock().remove(session_id) {
            handle.abort();
        }

        connection.demux().unregister_session_inbox(session_id);
        self.inner().admission.remove_session(session_id);

        connection.untrack_session(session_id);

        Ok(())
    }

    /// Subscribe to server notifications across all bound sessions.
    ///
    /// The server auto-subscribes harness sessions on `register_session`,
    /// so no wire request is needed. Call before `run()` — the
    /// parsing task is spawned inside `run()`.
    pub fn subscribe_notifications(&self) -> mpsc::Receiver<crate::notification::HubNotification> {
        let (event_tx, event_rx) = mpsc::channel::<crate::notification::HubNotification>(64);
        *self.inner().parsed_notif_tx.lock() = Some(event_tx);
        event_rx
    }

    /// Send a `tool.notify` frame to the server.
    ///
    /// Mirrors [`ToolHarness::send_notification`] but over a
    /// `tool_server` connection. The server allows `tool.notify` for both
    /// `Harness` and `ToolServer` connection kinds.
    ///
    /// The frame is fire-and-forget: this method returns `Ok` once the
    /// outbound message is queued, without waiting for a server ack.
    pub async fn send_notification(
        &self,
        notification: echo_agent_tool_protocol::ToolNotificationFrame,
    ) -> Result<(), ClientError> {
        let session = self
            .inner()
            .active_sessions
            .lock()
            .first()
            .cloned()
            .ok_or_else(|| {
                ClientError::InvalidConfig(
                    "send_notification requires at least one bound session".to_owned(),
                )
            })?;
        let connection = self.inner().borrow.connection();
        let request_id = connection.try_alloc_request_id()?;
        let req = echo_agent_tool_protocol::JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(session),
            method: Method::ToolNotify.as_wire_str().to_owned(),
            params: notification,
        };
        let text = serde_json::to_string(&req).map_err(ClientError::from)?;
        connection.send_outbound(text).await
    }

    /// Send a `system.notify` frame scoped to an explicit session and await the
    /// server's ack (a JSON-RPC request, unlike the fire-and-forget `send_notification`).
    pub async fn send_system_notification(
        &self,
        session_id: SessionId,
        params: echo_agent_tool_protocol::SystemNotifyParams,
    ) -> Result<SystemNotifyAck, ClientError> {
        // Fail fast on an oversized payload instead of round-tripping to the server.
        let payload_len = json_serialized_len(&params.payload)?;
        if payload_len > echo_agent_tool_protocol::MAX_SYSTEM_NOTIFY_PAYLOAD_BYTES {
            return Err(ClientError::ProtocolError(format!(
                "system.notify payload {payload_len} bytes exceeds {} byte cap",
                echo_agent_tool_protocol::MAX_SYSTEM_NOTIFY_PAYLOAD_BYTES
            )));
        }
        let connection = self.inner().borrow.connection();
        let request_id = connection.try_alloc_request_id()?;
        let req = echo_agent_tool_protocol::JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(session_id),
            method: Method::SystemNotify.as_wire_str().to_owned(),
            params,
        };
        let resp = connection.call_request(request_id, &req).await?;
        system_notify_ack_from_outcome(resp.outcome)
    }

    /// Backstop deadline for [`Self::request_hook`].
    ///
    /// Deliberately long: the hook is normally released by the real reply
    /// or requester teardown; this only bounds a request whose reply is
    /// lost (e.g. a dead connection), so it sits above any turn deadline.
    pub const HOOK_REQUEST_BACKSTOP_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(600);

    /// Send a request/response `Custom` hook on the shared connection and await its reply.
    ///
    /// The tool-server counterpart to the harness requester: the server
    /// originates the hook and the bound harness answers it. Bounded by
    /// [`HOOK_REQUEST_BACKSTOP_TIMEOUT`](Self::HOOK_REQUEST_BACKSTOP_TIMEOUT);
    /// use [`Self::request_hook_with_timeout`] for a different deadline.
    ///
    /// Only `permission_request` is answered by the bound harness today; any
    /// other `kind` is dropped by the responder and the call resolves only when
    /// the backstop timeout fires. Callers must pass a supported `kind`.
    pub async fn request_hook(
        &self,
        session_id: SessionId,
        kind: String,
        payload: Value,
    ) -> Result<Value, ClientError> {
        self.request_hook_with_timeout(
            session_id,
            kind,
            payload,
            Self::HOOK_REQUEST_BACKSTOP_TIMEOUT,
        )
        .await
    }

    /// [`Self::request_hook`] with a caller-supplied backstop deadline.
    pub async fn request_hook_with_timeout(
        &self,
        session_id: SessionId,
        kind: String,
        payload: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, ClientError> {
        let connection = self.inner().borrow.connection();
        // hook_id keys the server's parked-request table — must be globally unique.
        let hook_id = ToolCallId::new_v7().to_string();
        let hook = HookFrame::custom_request(session_id.clone(), hook_id, kind, payload);
        let request_id = connection.try_alloc_request_id()?;
        let req = echo_agent_tool_protocol::JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: Some(session_id),
            method: Method::Hook.as_wire_str().to_owned(),
            params: hook,
        };
        let resp = connection
            .call_request_with_timeout(request_id, &req, timeout)
            .await?;
        match resp.outcome {
            ResponseOutcome::Result(value) => Ok(value),
            ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
        }
    }

    /// Fire-and-forget `traces.donate`: `Ok` = queued; rejects surface
    /// only in server metrics. `otlp_request_b64` is a base64
    /// `ExportTraceServiceRequest` with a server-allowlisted `service.name`.
    pub async fn donate_traces(&self, otlp_request_b64: &str) -> Result<(), ClientError> {
        /// Borrowed wire shape so retried payloads are never cloned.
        #[derive(serde::Serialize)]
        struct ParamsRef<'a> {
            otlp_request: &'a str,
        }
        let session = self
            .inner()
            .active_sessions
            .lock()
            .first()
            .cloned()
            .ok_or_else(|| {
                ClientError::InvalidConfig(
                    "donate_traces requires at least one bound session".to_owned(),
                )
            })?;
        let connection = self.inner().borrow.connection();
        let notification = echo_agent_tool_protocol::JsonRpcNotification {
            jsonrpc: JsonRpcVersion,
            session_id: Some(session),
            seq: None,
            method: Method::TracesDonate.as_wire_str().to_owned(),
            params: ParamsRef {
                otlp_request: otlp_request_b64,
            },
        };
        let text = serde_json::to_string(&notification).map_err(ClientError::from)?;
        connection.send_outbound(text).await
    }

    /// Fire-and-forget `logs.donate`: `Ok` = queued; rejects surface
    /// only in server metrics. `otlp_request_b64` is a base64
    /// `ExportLogsServiceRequest` with a server-allowlisted `service.name`.
    /// Requires a bound session (mirrors [`Self::donate_traces`]).
    pub async fn donate_logs(&self, otlp_request_b64: &str) -> Result<(), ClientError> {
        /// Borrowed wire shape so retried payloads are never cloned.
        #[derive(serde::Serialize)]
        struct ParamsRef<'a> {
            otlp_request: &'a str,
        }
        let session = self
            .inner()
            .active_sessions
            .lock()
            .first()
            .cloned()
            .ok_or_else(|| {
                ClientError::InvalidConfig(
                    "donate_logs requires at least one bound session".to_owned(),
                )
            })?;
        let connection = self.inner().borrow.connection();
        let notification = echo_agent_tool_protocol::JsonRpcNotification {
            jsonrpc: JsonRpcVersion,
            session_id: Some(session),
            seq: None,
            method: Method::LogsDonate.as_wire_str().to_owned(),
            params: ParamsRef {
                otlp_request: otlp_request_b64,
            },
        };
        let text = serde_json::to_string(&notification).map_err(ClientError::from)?;
        connection.send_outbound(text).await
    }

    /// Fire-and-forget `metrics.donate`: `Ok` = queued; rejects surface
    /// only in server metrics. `otlp_request_b64` is a base64
    /// `ExportMetricsServiceRequest` with a server-allowlisted `service.name`.
    /// Unlike [`Self::donate_logs`], metrics are process-aggregate, so
    /// this does **not** require a bound session.
    pub async fn donate_metrics(&self, otlp_request_b64: &str) -> Result<(), ClientError> {
        /// Borrowed wire shape so retried payloads are never cloned.
        #[derive(serde::Serialize)]
        struct ParamsRef<'a> {
            otlp_request: &'a str,
        }
        let connection = self.inner().borrow.connection();
        let notification = echo_agent_tool_protocol::JsonRpcNotification {
            jsonrpc: JsonRpcVersion,
            session_id: None,
            seq: None,
            method: Method::MetricsDonate.as_wire_str().to_owned(),
            params: ParamsRef {
                otlp_request: otlp_request_b64,
            },
        };
        let text = serde_json::to_string(&notification).map_err(ClientError::from)?;
        connection.send_outbound(text).await
    }

    /// Drive the inbound loop until either the connection actor
    /// signals shutdown OR [`Self::shutdown`] is called.
    ///
    /// Each bound session gets its own per-session task that pulls
    /// frames from the connection's demux and dispatches them to the
    /// matching handler by `tool_id`.
    pub async fn run(&self) -> Result<(), ClientError> {
        let connection = self.inner().borrow.connection().clone();
        let demux = connection.demux();

        for sid in &self.inner().initial_sessions {
            if let Err(e) = self.bind_session_local(sid.clone()).await {
                warn!(%sid, error = %e, "run: bind_session_local failed for builder session");
                continue;
            }
            // Publish the tool snapshot so the server registers the tools
            // for this session. bind_session_local only does local setup.
            if let Err(e) = self.serve(sid.clone()).await {
                warn!(%sid, error = %e, "run: serve failed for builder session");
            }
        }

        // If subscribe_notifications() was called before run(),
        // install the forwarding channel and spawn the parsing task.
        if let Some(event_tx) = self.inner().parsed_notif_tx.lock().take() {
            let (fwd_tx, mut fwd_rx) = mpsc::channel::<Value>(64);
            *self.inner().notification_fwd.lock() = Some(fwd_tx);
            tokio::spawn(async move {
                while let Some(value) = fwd_rx.recv().await {
                    if let Some(event) = crate::notification::HubNotification::parse(&value)
                        && event_tx.send(event).await.is_err()
                    {
                        break;
                    }
                }
            });
        }

        let mut notif_rx = connection
            .take_early_notifications()
            .unwrap_or_else(|| demux.subscribe_notifications());
        let buffered = notif_rx.len();
        if buffered > 0 {
            crate::metrics::early_notif_buffered(buffered as u64);
            tracing::info!(
                buffered,
                "replaying connection-level notifications buffered before run()"
            );
        }
        // Wrap in Arc so spawned per-bind tasks hold Arc::clone
        // (refcount bump) instead of ToolServer::clone(). A
        // ToolServer::clone() going out of scope triggers
        // Drop::begin_teardown() on the shared AtomicBool, which
        // tears down *all* sessions as soon as the first spawned
        // bind task completes.
        let server_for_notif = Arc::new(self.clone());
        let connection_for_notif = connection.clone();
        let notif_handle = tokio::spawn(async move {
            loop {
                let frame = match notif_rx.recv().await {
                    Ok(frame) => frame,
                    Err(RecvError::Lagged(skipped)) => {
                        crate::metrics::notif_lagged_recovered();
                        tracing::warn!(
                            skipped,
                            "connection notification stream lagged; continuing"
                        );
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                };
                let method = frame
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let request_id = frame.get("id").filter(|v| !v.is_null()).cloned();

                match method {
                    "session.bind" => {
                        let Some(sid_str) = frame
                            .pointer("/params/session_id")
                            .and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        let Ok(sid) = echo_agent_tool_protocol::SessionId::new(sid_str) else {
                            continue;
                        };
                        tracing::info!(%sid, "session.bind: binding new session");

                        let bind_params = frame.get("params").cloned();
                        let server = Arc::clone(&server_for_notif);
                        let conn = connection_for_notif.clone();
                        tokio::spawn(async move {
                            let result = server
                                .bind_session_local_with_metadata(sid.clone(), bind_params)
                                .await;

                            // Respond with tools on success, error on failure.
                            // The server registers tools from this response directly
                            // (v2 protocol — no separate serve RPC needed).
                            if let Some(id) = request_id {
                                let response = match result {
                                    Ok(()) => {
                                        let tools: Vec<echo_agent_tool_types::ToolDescription> =
                                            server
                                                .handlers_for_session(&sid)
                                                .iter()
                                                .map(|h| h.description())
                                                .collect();
                                        let result = echo_agent_tool_protocol::SessionBindResult {
                                            tools,
                                            binary_version: server.inner().binary_version.clone(),
                                            unserved_tool_ids: server.unserved_for_session(&sid),
                                            resolve_error: server.resolve_error_for_session(&sid),
                                            image_capabilities: server
                                                .inner()
                                                .image_capabilities
                                                .clone(),
                                        };
                                        serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "result": result
                                        })
                                    }
                                    Err(ref e) => {
                                        tracing::warn!(
                                            error = %e, session = %sid,
                                            "session.bind: bind_session_local failed"
                                        );
                                        // Resolver failures carry a decodable
                                        // ToolErrorWire; forward its numeric
                                        // code + payload so the cause survives
                                        // past the server instead of collapsing
                                        // to a bare -32603.
                                        let (code, data) = match e {
                                            ClientError::Wire(wire) => (
                                                error_codes::from_tool_error_wire(wire),
                                                serde_json::to_value(wire).ok(),
                                            ),
                                            _ => (-32603, None),
                                        };
                                        serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "id": id,
                                            "error": {
                                                "code": code,
                                                "message": format!("bind failed: {e}"),
                                                "data": data
                                            }
                                        })
                                    }
                                };
                                if let Ok(text) = serde_json::to_string(&response)
                                    && let Err(e) = conn.send_outbound(text).await
                                {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to send session.bind response"
                                    );
                                }
                            }
                        });
                    }
                    "session.unbind" => {
                        let Some(sid_str) = frame
                            .pointer("/params/session_id")
                            .and_then(serde_json::Value::as_str)
                        else {
                            continue;
                        };
                        let Ok(sid) = echo_agent_tool_protocol::SessionId::new(sid_str) else {
                            continue;
                        };
                        tracing::info!(%sid, "session.unbind: unbinding session");

                        let server = server_for_notif.clone();
                        let conn = connection_for_notif.clone();
                        tokio::spawn(async move {
                            // Unbind precedes hibernate: flush while up.
                            server.flush_donations().await;
                            let _ = server.unbind_session(&sid).await;

                            // Respond so session.close returns synchronously.
                            if let Some(id) = request_id {
                                let response = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {}
                                });
                                if let Ok(text) = serde_json::to_string(&response)
                                    && let Err(e) = conn.send_outbound(text).await
                                {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to send session.unbind response"
                                    );
                                }
                            }
                        });
                    }
                    // Server-issued graceful-shutdown request, fanned out to the
                    // evicted session's handlers (mirroring the hook fan-out).
                    "tool_server.evict" => {
                        let Some(params) = frame.get("params") else {
                            continue;
                        };
                        // Deserialize straight from the borrowed `&Value`
                        // (`&Value: Deserializer`) — no need to clone the params.
                        let evict: ToolServerEvictParams =
                            match <ToolServerEvictParams as serde::Deserialize>::deserialize(params)
                            {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "tool_server.evict: failed to decode params"
                                    );
                                    continue;
                                }
                            };
                        tracing::info!(
                            session = %evict.session_id,
                            reason = %evict.reason,
                            grace_period_ms = evict.grace_period_ms,
                            "tool_server.evict received; draining"
                        );
                        let server = Arc::clone(&server_for_notif);
                        let conn = connection_for_notif.clone();
                        tokio::spawn(async move {
                            // Ack first (best-effort) so the server's request
                            // resolves promptly; the drain runs in the background.
                            if let Some(id) = request_id {
                                let response = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {}
                                });
                                if let Ok(text) = serde_json::to_string(&response)
                                    && let Err(e) = conn.send_outbound(text).await
                                {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to send tool_server.evict ack"
                                    );
                                }
                            }
                            for handler in server.handlers_for_session(&evict.session_id) {
                                handler.handle_evict(evict.clone()).await;
                            }
                        });
                    }
                    _ => {}
                }
            }
        });

        // Spawn a task that replays `serve` for every active session
        // after a reconnect. The on_reconnect callback (sync) signals
        // via Notify; this async task picks up the event and does
        // the actual serve calls, then fires on_reconnect_settled so
        // readiness markers can wait until tools are re-served.
        let server_for_reconnect = Arc::new(self.clone());
        let reconnect_handle = {
            let server = server_for_reconnect;
            let notify = Arc::clone(&self.inner().reconnect_notify);
            let settled = self.inner().on_reconnect_settled.clone();
            let epoch = Arc::clone(&self.inner().disconnect_epoch);
            tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    // Snapshot the disconnect epoch for the connection we are
                    // now replaying onto; if it advances during replay a fresh
                    // disconnect or terminal close raced us and `settled` must
                    // not fire (it would resurrect a stale ready marker over a
                    // downed socket).
                    let epoch_at_start = epoch.load(Ordering::Acquire);
                    let sessions: Vec<SessionId> = server.active_sessions();
                    tracing::info!(
                        sessions = sessions.len(),
                        "reconnect: replaying serve for active sessions"
                    );
                    let mut all_served = true;
                    for sid in sessions {
                        if let Err(e) = server.serve(sid.clone()).await {
                            all_served = false;
                            tracing::warn!(
                                error = %e,
                                session = %sid,
                                "reconnect: serve replay failed"
                            );
                        }
                    }
                    // Only settle when every session re-served AND no disconnect
                    // or terminal close raced this replay — otherwise the next
                    // reconnect's notify re-runs this loop and settles then.
                    let raced = epoch.load(Ordering::Acquire) != epoch_at_start;
                    if !all_served || raced {
                        tracing::info!(
                            all_served,
                            raced,
                            "reconnect: not settling ready (replay incomplete or disconnect raced)"
                        );
                        continue;
                    }
                    if let Some(ref cb) = settled {
                        cb();
                    }
                }
            })
        };

        let shutdown = self.inner().borrow.shutdown_token().clone();
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {}
            _ = connection.await_shutdown() => {}
        }
        notif_handle.abort();
        reconnect_handle.abort();
        for (_, handle) in self.inner().session_handles.lock().drain() {
            handle.abort();
        }
        Ok(())
    }

    /// Cooperatively shut the server down: signal `run` to return,
    /// unregister each session binding (refcount-aware), and unbind
    /// each registered tool.
    ///
    /// Errors during teardown are aggregated rather than short-
    /// circuiting so a partial cleanup still releases everything it
    /// can. The first error (if any) is returned.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        // Mark torn_down BEFORE the cleanup so the Drop fallback
        // doesn't double-schedule.
        if !self.inner().borrow.begin_teardown() {
            return Ok(());
        }
        // Real teardown: flush AND clear the pumps to break the reference cycle.
        flush_donations_inner(self.inner().as_ref(), true).await;
        teardown_sessions(self.inner().as_ref()).await;
        Ok(())
    }

    pub(crate) fn set_donation_pump(&self, tx: mpsc::Sender<crate::donate_pump::PumpMsg>) {
        self.inner().donation_pumps.lock().traces = Some(tx);
    }

    pub(crate) fn set_log_donation_pump(&self, tx: mpsc::Sender<crate::donate_pump::PumpMsg>) {
        self.inner().donation_pumps.lock().logs = Some(tx);
    }

    #[cfg(feature = "metrics")]
    pub(crate) fn set_metric_donation_pump(&self, tx: mpsc::Sender<crate::donate_pump::PumpMsg>) {
        self.inner().donation_pumps.lock().metrics = Some(tx);
    }

    /// Clone of the connection's shutdown token so the periodic metric
    /// reporter (the only perpetually-running donation task) can stop on
    /// teardown instead of gathering and sending forever.
    #[cfg(feature = "metrics")]
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.inner().borrow.shutdown_token().clone()
    }

    /// Flush each producer and fence its donation pump; no-op without a
    /// pump. Drives all three signals so a teardown never abandons a batch.
    pub async fn flush_donations(&self) {
        flush_donations_inner(self.inner().as_ref(), false).await;
    }
}

/// Fence each donation pump. `clear_pumps` only from `shutdown` / `Drop`.
async fn flush_donations_inner(inner: &ToolServerInner, clear_pumps: bool) {
    let (traces, logs, metrics) = {
        let pumps = inner.donation_pumps.lock();
        (
            pumps.traces.clone(),
            pumps.logs.clone(),
            pumps.metrics.clone(),
        )
    };
    if let Some(tx) = traces {
        fastrace::flush();
        crate::donate_pump::drain_via(&tx).await;
    }
    if let Some(tx) = logs {
        crate::log_donate::flush_log_layer();
        crate::donate_pump::drain_via(&tx).await;
    }
    if let Some(tx) = metrics {
        #[cfg(feature = "metrics")]
        crate::metric_donate::gather_and_send();
        crate::donate_pump::drain_via(&tx).await;
    }

    // Teardown only: drop pump senders so pump tasks exit. Keep them on
    // while-running unbind flushes (`clear_pumps = false`).
    if clear_pumps {
        {
            let mut pumps = inner.donation_pumps.lock();
            pumps.traces = None;
            pumps.logs = None;
            pumps.metrics = None;
        }
        #[cfg(feature = "metrics")]
        crate::metric_donate::clear_active_exporter();
    }
}

impl Drop for ToolServer {
    fn drop(&mut self) {
        // `into_inner` (not `try_unwrap`): exactly one concurrent dropper wins.
        let Some(inner) = self.inner.take() else {
            return;
        };
        let Some(owned) = Arc::into_inner(inner) else {
            return;
        };
        if !owned.borrow.begin_teardown() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                flush_donations_inner(&owned, true).await;
                teardown_sessions(&owned).await;
            });
        }
    }
}

/// Shared teardown for `shutdown` and `Drop`. Drain-and-cancel every
/// session's in-flight tokens BEFORE aborting the dispatchers, so detached
/// `execute_call` tasks wind down via their `select!` rather than being
/// orphaned, then unregister inboxes, drop admission entries,
/// and untrack sessions. Callers own the `begin_teardown` guard.
async fn teardown_sessions(inner: &ToolServerInner) {
    let connection = inner.borrow.connection();
    let all_sessions: Vec<SessionId> = inner.active_sessions.lock().clone();

    push_disconnect_status(connection, &all_sessions).await;
    inner.borrow.shutdown_token().cancel();

    for entry in inner.cancels.iter() {
        entry.value().cancel_all();
    }
    inner.cancels.clear();
    for (_, handle) in inner.session_handles.lock().drain() {
        handle.abort();
    }
    let demux = connection.demux();
    for sid in &all_sessions {
        let _ = demux.unregister_session_inbox(sid);
        inner.admission.remove_session(sid);
        connection.untrack_session(sid);
    }
}

/// Push `tool_server.status(Disconnected)` for each bound session.
/// Must be called before `unregister_session` (the server needs the
/// session bindings to route the notification).
async fn push_disconnect_status(connection: &HubConnection, sessions: &[SessionId]) {
    use echo_agent_tool_protocol::{
        JsonRpcRequest, ToolServerLifecycleStatus, ToolServerStatusPayload,
    };

    for sid in sessions {
        let mut payload =
            ToolServerStatusPayload::terminal(ToolServerLifecycleStatus::Disconnected);
        payload.session_id = Some(sid.clone());
        let params = match serde_json::to_value(&payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Ok(request_id) = connection.try_alloc_request_id() else {
            continue;
        };
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: None,
            method: Method::ToolServerStatus.as_wire_str().to_owned(),
            params,
        };
        let text = match serde_json::to_string(&req) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if let Err(e) = connection.send_outbound(text).await {
            debug!(error = %e, session = %sid, "push_disconnect_status failed");
        }
    }
}

/// Per-session inbound dispatcher.
///
/// Dequeues one frame at a time. Notifications (including `Cancel`
/// hooks) are handled **inline** because they are cheap and must never
/// queue behind a running call. A `tool_call_request` is dispatched to a
/// spawned task that performs three-tier admission before running, and
/// the loop immediately returns to `rx.recv()`, so calls within a session
/// run concurrently.
async fn run_session_loop(
    session_id: SessionId,
    mut rx: mpsc::Receiver<InboundFrame>,
    connection: Arc<HubConnection>,
    session_handlers: SessionHandlerMap,
    notification_fwd: Arc<parking_lot::Mutex<Option<mpsc::Sender<Value>>>>,
    admission: Arc<crate::admission::Admission>,
    cancels: Arc<CancelRegistry>,
    cancels_owner: Arc<DashMap<SessionId, Arc<CancelRegistry>>>,
) {
    while let Some(frame) = rx.recv().await {
        let handlers = session_handlers
            .read()
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        match frame {
            // Cheap, inline — keeps Cancel ahead of running calls.
            InboundFrame::Notification(value) => {
                handle_notification(
                    &session_id,
                    value,
                    &handlers,
                    &notification_fwd,
                    &cancels,
                    &connection,
                )
                .await;
            }
            // Hot path: never await execution in the loop. Admission runs
            // *inside* the spawned task (acquiring before spawn would
            // head-of-line block the loop and Cancel hooks).
            InboundFrame::Request(value) => {
                // Register the cancellation token BEFORE spawn
                // so an inline `Cancel` dequeued immediately after can never
                // race ahead of registration and silently no-op. A pending
                // tombstone (cancel-before-registration) pre-cancels here.
                let token = CancellationToken::new();
                let call_id = parse_tool_call_id(&value);
                if let Some(id) = &call_id {
                    cancels.register(id.clone(), &token);
                }
                let sid = session_id.clone();
                let conn = connection.clone();
                let adm = admission.clone();
                let cancels = cancels.clone();
                tokio::spawn(async move {
                    execute_call(&sid, value, &conn, &handlers, &adm, token).await;
                    // Always deregister on completion/cancel,
                    // regardless of which path `execute_call` returned by.
                    if let Some(id) = call_id {
                        cancels.deregister(&id);
                    }
                });
            }
        }
    }
    // Loop exited (inbox closed): the session is being torn down, so wind
    // down this loop's own in-flight detached calls. Doing it here makes
    // exit self-sufficient — it never depends on the rebind path's
    // `insert -> old.cancel_all()` running (which can be skipped if this
    // exit removes the entry first). `cancel_all` on an empty/already-closed
    // registry is a safe no-op.
    cancels.cancel_all();
    // Symmetric cleanup of BOTH per-session entries. `remove_if` guards
    // against evicting a fresh registry that a concurrent rebind just
    // installed (only this loop's own `Arc` is removed). Unbind/shutdown
    // also remove these when they abort the loop.
    admission.remove_session(&session_id);
    cancels_owner.remove_if(&session_id, |_, registry| Arc::ptr_eq(registry, &cancels));
}

/// Extract the `params.tool_call_id` from a raw `tool_call_request`
/// frame so the dispatcher can register a cancellation token under it
/// before spawning the call.
fn parse_tool_call_id(value: &Value) -> Option<ToolCallId> {
    value
        .pointer("/params/tool_call_id")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Dispatch an inbound notification frame.
///
/// Hook frames (`method == "hook"`) are forwarded to every registered
/// handler whose `tool_id` matches the frame's target, or to every
/// handler when the hook is session-wide (no `tool_id`).
///
/// All other notifications (e.g. `ToolsChanged`, `tool.notification`)
/// are forwarded to the `notification_fwd` channel if one has been
/// installed by [`ToolServer::subscribe_notifications`]. This lets
/// the session loop (which owns the demux inbox) coexist with the
/// notification subscriber without a registration conflict.
async fn handle_notification(
    session_id: &SessionId,
    value: Value,
    handlers: &[Arc<dyn ToolServerHandler>],
    notification_fwd: &parking_lot::Mutex<Option<mpsc::Sender<Value>>>,
    cancels: &CancelRegistry,
    connection: &Arc<HubConnection>,
) {
    let method = value.get("method").and_then(Value::as_str).unwrap_or("");
    if method == Method::Hook.as_wire_str() {
        let Some(params) = value.get("params").cloned() else {
            warn!(%session_id, "hook notification missing params; ignoring");
            return;
        };
        let frame: HookFrame = match serde_json::from_value(params) {
            Ok(f) => f,
            Err(err) => {
                warn!(?err, %session_id, "hook notification failed to decode");
                return;
            }
        };
        let hook_span = frame
            .trace_context
            .as_deref()
            .and_then(fastrace::collector::SpanContext::decode_w3c_traceparent)
            .map(|parent| {
                // Lets the server scope this span to its owning session.
                fastrace::Span::root("tool_server.hook", parent)
                    .with_property(|| ("session_id", session_id.as_str().to_owned()))
            })
            .unwrap_or_else(fastrace::Span::noop);
        if let Some(hook_id) = frame.hook_id.clone() {
            use fastrace::future::FutureExt as _;
            // Spawned: a hook-request handler may legitimately take seconds
            // (e.g. a handler that enqueues follow-up work for its `After`
            // ack); inline it would head-of-line block this loop and Cancel
            // hooks. Correlation rides `hook_id`, so ordering is not
            // load-bearing.
            let session_id = session_id.clone();
            let handlers = handlers.to_vec();
            let connection = connection.clone();
            tokio::spawn(
                async move {
                    let mut result = None;
                    for handler in &handlers {
                        if let Some(r) = handler
                            .handle_hook_request(session_id.clone(), frame.clone())
                            .await
                        {
                            result = Some(r);
                            break;
                        }
                    }
                    let notif = JsonRpcNotification {
                        jsonrpc: JsonRpcVersion,
                        session_id: Some(session_id.clone()),
                        seq: None,
                        method: Method::HookReply.as_wire_str().to_owned(),
                        params: HookReplyFrame {
                            session_id: session_id.clone(),
                            hook_id,
                            result: result.unwrap_or(Value::Null),
                        },
                    };
                    match serde_json::to_string(&notif) {
                        Ok(text) => {
                            let _ = connection.send_outbound(text).await;
                        }
                        Err(err) => warn!(?err, %session_id, "failed to encode hook_reply"),
                    }
                }
                .in_span(hook_span),
            );
            return;
        }
        // Apply `Cancel` BEFORE the per-handler `handle_hook` fan-out
        // — a slow user `handle_hook` must never delay the
        // cancel. Compiler-enforced exhaustiveness keeps new HookEvent
        // variants from silently skipping this dispatch.
        match &frame.event {
            HookEvent::Cancel => {
                crate::metrics::cancel_hook_received();
                if let Some(call_id) = &frame.call_id {
                    if cancels.cancel(call_id) {
                        crate::metrics::cancel_applied();
                    } else {
                        crate::metrics::cancel_pending_tombstoned();
                    }
                } else {
                    crate::metrics::cancel_no_target();
                }
            }
            HookEvent::Pause
            | HookEvent::Resume
            | HookEvent::SessionEnded
            | HookEvent::Custom { .. } => {}
        }
        let frame_tool_id = frame.tool_id.clone();
        {
            use fastrace::future::FutureExt as _;
            async {
                for handler in handlers {
                    if let Some(target) = &frame_tool_id
                        && handler.tool_id() != *target
                    {
                        continue;
                    }
                    handler.handle_hook(session_id.clone(), frame.clone()).await;
                }
            }
            .in_span(hook_span)
            .await;
        }
        return;
    }

    // Forward to notification subscriber if one is registered.
    if let Some(fwd) = notification_fwd.lock().as_ref() {
        if fwd.try_send(value).is_err() {
            warn!(%session_id, "notification forwarding channel full or closed");
        }
        return;
    }
    debug!(?value, %session_id, method = %method, "tool-server received notification (no subscriber)");
}

/// Execute one `tool_call_request`: parse id/params, admit via the
/// [`Admission`](crate::admission::Admission) controller, locate the
/// handler, build the call context, drain the handler stream
/// (forwarding progress), build the JSON-RPC response, and ship it
/// back. Invoked from a spawned task by the dispatcher.
///
/// Admission happens AFTER parsing id/params (so an overload can be
/// addressed to the request) but BEFORE invoking the handler. On
/// overload it emits the shared `-32016` "tool_busy" error — never a
/// silent drop — and the `AdmitGuard` holds all three permits for the
/// handler's lifetime, releasing them on return.
///
/// `token` is the per-call cancellation handle registered by the
/// dispatcher before spawn. It is exposed to the tool via
/// the [`Cancellation`] extension and the handler-stream drain is wrapped
/// in a biased `select!` on it, so a `Cancel` hook hard-cancels by
/// dropping the call future and yields a `ToolError::Cancelled` response.
async fn execute_call(
    session_id: &SessionId,
    value: Value,
    connection: &Arc<HubConnection>,
    handlers: &[Arc<dyn ToolServerHandler>],
    admission: &crate::admission::Admission,
    token: CancellationToken,
) {
    let id_value = match value.get("id").cloned() {
        Some(v) => v,
        None => {
            warn!("tool_call_request missing id; ignoring");
            return;
        }
    };
    let json_id: JsonRpcId = match serde_json::from_value(id_value) {
        Ok(v) => v,
        Err(err) => {
            warn!(?err, "tool_call_request id failed to decode");
            return;
        }
    };
    let params: ToolCallParams = match value
        .get("params")
        .cloned()
        .and_then(|p| serde_json::from_value(p).ok())
    {
        Some(p) => p,
        None => {
            send_error(
                connection,
                json_id,
                session_id.clone(),
                -32602,
                "invalid params",
            )
            .await;
            return;
        }
    };
    let Some(handler) = handlers.iter().find(|h| h.tool_id() == params.tool_id) else {
        crate::metrics::no_handler();
        send_error(
            connection,
            json_id,
            session_id.clone(),
            -32011,
            &format!("no handler for tool_id {}", params.tool_id),
        )
        .await;
        return;
    };

    // Admission held for the handler's lifetime; overload -> -32016 reply.
    let _guard = match admission.admit(session_id).await {
        Ok(guard) => guard,
        Err(crate::admission::Overloaded::Timeout) => {
            crate::metrics::tool_call_rejected_overloaded();
            send_overloaded(connection, json_id, session_id.clone()).await;
            return;
        }
        Err(crate::admission::Overloaded::Shutdown) => return,
    };

    let call_span = params
        .trace_context
        .as_deref()
        .and_then(fastrace::collector::SpanContext::decode_w3c_traceparent)
        .map(|parent| {
            fastrace::Span::root("tool_server.tool_call", parent).with_properties(|| {
                [
                    ("tool_id", params.tool_id.as_str().to_owned()),
                    ("tool_call_id", params.tool_call_id.as_str().to_owned()),
                    // Lets the server scope this span to its owning session.
                    ("session_id", session_id.as_str().to_owned()),
                ]
            })
        })
        .unwrap_or_else(fastrace::Span::noop);

    let mut ctx = ToolCallContext::new(params.tool_call_id.clone());
    ctx.extensions
        .insert(echo_agent_tool_runtime::SessionContext(
            session_id.as_str().to_owned(),
        ));
    if let Some(cwd) = params.cwd {
        ctx.extensions.insert(Cwd(std::path::PathBuf::from(cwd)));
    }
    if let Some(version) = params.behavior_version {
        ctx.extensions.insert(BehaviorVersion(version));
    }
    if let Some(trace) = params.trace_context {
        ctx.extensions.insert(TraceContext(trace));
    }
    // Expose the cancellation handle so cooperative tools can poll/await
    // it; the dispatcher still hard-cancels via the `select!` below.
    ctx.extensions.insert(Cancellation(token.clone()));

    // Move `tool_id` (unused afterward) into the cancellation arm; clone
    // only `tool_call_id`, which the response build still needs.
    let tool_id = params.tool_id;
    let tool_call_id = params.tool_call_id.clone();
    let arguments = params.arguments;

    // Drain the handler stream, forwarding progress and capturing the
    // terminal. Wrapped in a biased `select!` on the cancellation token
    // so a `Cancel` DROPS this future (hard cancel of the in-flight
    // await) and yields a `ToolError::Cancelled` response.
    let drain = async {
        let mut stream = handler.handle_call(ctx, arguments).await;
        let mut terminal: Option<Result<TypedToolOutput, ToolError>> = None;
        while let Some(item) = stream.next().await {
            match item {
                ToolStreamItem::Progress(progress) => {
                    let frame = progress_to_frame(progress, tool_call_id.clone());
                    let notif = JsonRpcNotification {
                        jsonrpc: JsonRpcVersion,
                        session_id: Some(session_id.clone()),
                        seq: None,
                        method: Method::ToolCallProgress.as_wire_str().to_owned(),
                        params: frame,
                    };
                    if let Ok(text) = serde_json::to_string(&notif) {
                        crate::metrics::progress_frame_forwarded();
                        let _ = connection.send_outbound(text).await;
                    }
                }
                ToolStreamItem::Terminal(result) => {
                    terminal = Some(result);
                    break;
                }
            }
        }
        terminal
    };
    // Span duration = actual tool execution (the per-hop leaf).
    let drain = {
        use fastrace::future::FutureExt as _;
        drain.in_span(call_span)
    };
    let terminal: Option<Result<TypedToolOutput, ToolError>> = tokio::select! {
        biased;
        _ = token.cancelled() => Some(Err(ToolError::cancelled(
            tool_id,
            "tool call cancelled by server",
        ))),
        t = drain => t,
    };

    let response = match terminal {
        Some(Ok(typed)) => {
            // A cco encode failure degrades to `None` (mirroring the decode
            // side) rather than failing an otherwise-successful call.
            let chat_completion_output = typed.chat_completion_output.as_ref().and_then(|cco| {
                serde_json::to_value(cco)
                    .inspect_err(|err| warn!(%err, "dropping unencodable chat_completion_output"))
                    .ok()
            });
            let encoded = serde_json::to_value(ToolCallResult {
                tool_call_id: params.tool_call_id.clone(),
                output: ToolOutputWire::Json(typed.value),
                follow_ups: Vec::new(),
                reminders: Vec::new(),
                chat_completion_output,
            });
            match encoded {
                Ok(payload) => JsonRpcResponse {
                    jsonrpc: JsonRpcVersion,
                    id: json_id,
                    session_id: Some(session_id.clone()),
                    outcome: ResponseOutcome::Result(payload),
                },
                Err(err) => {
                    let message = format!("failed to encode tool_call_result: {err}");
                    JsonRpcResponse {
                        jsonrpc: JsonRpcVersion,
                        id: json_id,
                        session_id: Some(session_id.clone()),
                        outcome: ResponseOutcome::Error(JsonRpcError {
                            code: -32603,
                            data: serde_json::to_value(ToolErrorWire::Internal {
                                request_id: None,
                                detail: Some(message.clone()),
                            })
                            .ok(),
                            message,
                        }),
                    }
                }
            }
        }
        Some(Err(err)) => build_error_response(json_id, session_id.clone(), err),
        None => {
            warn!("handler stream ended without terminal");
            JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: json_id,
                session_id: Some(session_id.clone()),
                outcome: ResponseOutcome::Error(JsonRpcError {
                    code: -32603,
                    message: "handler produced no terminal".to_owned(),
                    data: serde_json::to_value(ToolErrorWire::Internal {
                        request_id: None,
                        detail: Some("handler produced no terminal".to_owned()),
                    })
                    .ok(),
                }),
            }
        }
    };

    let text = match serde_json::to_string(&response) {
        Ok(s) => s,
        Err(err) => {
            warn!(?err, "failed to serialise tool_call_result; dropping");
            return;
        }
    };
    if let Err(err) = connection.send_outbound(text).await {
        warn!(?err, "failed to send tool_call_result");
    }
}

/// Convert a [`ToolProgress`] into a wire [`ToolCallProgressFrame`].
fn progress_to_frame(progress: ToolProgress, tool_call_id: ToolCallId) -> ToolCallProgressFrame {
    let (kind, body) = match progress {
        ToolProgress::Text { text } => ("text".to_owned(), serde_json::json!({ "text": text })),
        ToolProgress::Content { blocks } => (
            "content".to_owned(),
            serde_json::to_value(blocks).unwrap_or_else(|err| {
                warn!(
                    ?err,
                    "failed to serialize Content blocks for progress frame"
                );
                Value::default()
            }),
        ),
        ToolProgress::Custom { subkind, payload } => (subkind, payload),
    };
    ToolCallProgressFrame {
        tool_call_id,
        kind,
        body,
        dropped_count: None,
    }
}

/// Build the error response for a failed tool call, preserving the full
/// [`ToolError`] as a decodable [`ToolErrorWire`] in `error.data` (and the
/// matching numeric code) so the harness recovers kind + detail + structured
/// details instead of collapsing everything to a bare `-32603` string.
fn build_error_response(id: JsonRpcId, session_id: SessionId, err: ToolError) -> JsonRpcResponse {
    let message = err.to_string();
    let wire = ToolErrorWire::from(err);
    JsonRpcResponse {
        jsonrpc: JsonRpcVersion,
        id,
        session_id: Some(session_id),
        outcome: ResponseOutcome::Error(JsonRpcError {
            code: error_codes::from_tool_error_wire(&wire),
            message,
            data: serde_json::to_value(&wire).ok(),
        }),
    }
}

async fn send_error(
    connection: &Arc<HubConnection>,
    id: JsonRpcId,
    session_id: SessionId,
    code: i32,
    message: &str,
) {
    send_error_with_data(connection, id, session_id, code, message, None).await;
}

/// Like [`send_error`] but attaches a machine-readable `data` payload so
/// receivers can switch on `data.code` (see `error_codes.rs`).
async fn send_error_with_data(
    connection: &Arc<HubConnection>,
    id: JsonRpcId,
    session_id: SessionId,
    code: i32,
    message: &str,
    data: Option<Value>,
) {
    let response = JsonRpcResponse::<Value> {
        jsonrpc: JsonRpcVersion,
        id,
        session_id: Some(session_id),
        outcome: ResponseOutcome::Error(JsonRpcError {
            code,
            message: message.to_owned(),
            data,
        }),
    };
    if let Ok(text) = serde_json::to_string(&response) {
        let _ = connection.send_outbound(text).await;
    }
}

/// Ship the shared overloaded (-32016 "tool_busy") response built by
/// [`crate::admission::overloaded_response`] — the single source of the
/// overload wire shape, reused by the demux inbox-full path too.
async fn send_overloaded(connection: &Arc<HubConnection>, id: JsonRpcId, session_id: SessionId) {
    let response = crate::admission::overloaded_response(id, session_id);
    if let Ok(text) = serde_json::to_string(&response) {
        let _ = connection.send_outbound(text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tool_runtime::ContentBlock;

    fn call_id() -> ToolCallId {
        ToolCallId::new_v7()
    }

    #[test]
    fn tool_server_tuning_does_not_override_attempt_reset_dwell() {
        let tuning = ToolServerBuilder::default().connection_tuning();
        assert_eq!(
            tuning.reconnect_attempt_reset_after, None,
            "builder must pass None so connect() applies the 10s default"
        );
        assert_eq!(
            crate::connection::resolve_attempt_reset_after(tuning.reconnect_attempt_reset_after),
            std::time::Duration::from_secs(10),
        );
    }

    // ── build_error_response: wire fidelity ─────────────────────────

    #[test]
    fn build_error_response_preserves_kind_and_details_in_data() {
        let sid = SessionId::new("sess").expect("valid");
        let tool = ToolId::new("run_terminal_command").expect("valid");
        let err = ToolError::execution(tool, "command exited 127: bash: not found");
        let resp = build_error_response(JsonRpcId::Number(1), sid, err);
        match resp.outcome {
            ResponseOutcome::Error(rpc) => {
                assert_eq!(rpc.code, -32603, "Execution maps to internal_error numeric");
                assert!(rpc.message.contains("command exited 127"));
                let data = rpc.data.expect("ToolErrorWire payload must ride in data");
                let wire: ToolErrorWire =
                    serde_json::from_value(data).expect("data decodes as ToolErrorWire");
                match wire {
                    ToolErrorWire::Execution { tool_id, message } => {
                        assert_eq!(tool_id.as_str(), "run_terminal_command");
                        assert_eq!(message, "command exited 127: bash: not found");
                    }
                    other => panic!("expected Execution, got {other:?}"),
                }
            }
            ResponseOutcome::Result(_) => panic!("expected error outcome"),
        }
    }

    #[test]
    fn build_error_response_maps_invalid_arguments_numeric() {
        let sid = SessionId::new("sess").expect("valid");
        let err = ToolError::invalid_arguments("missing `path`");
        let resp = build_error_response(JsonRpcId::Number(2), sid, err);
        match resp.outcome {
            ResponseOutcome::Error(rpc) => {
                assert_eq!(rpc.code, -32602, "InvalidArguments maps to invalid_params");
                assert_eq!(
                    rpc.data
                        .expect("data present")
                        .get("code")
                        .and_then(|v| v.as_str()),
                    Some("invalid_params"),
                );
            }
            ResponseOutcome::Result(_) => panic!("expected error outcome"),
        }
    }

    // ── progress_to_frame: basic variant tests ──────────────────────

    #[test]
    fn progress_text_produces_text_kind_frame() {
        let id = call_id();
        let progress = ToolProgress::Text {
            text: "hello world".to_owned(),
        };
        let frame = progress_to_frame(progress, id.clone());
        assert_eq!(frame.tool_call_id, id);
        assert_eq!(frame.kind, "text");
        assert_eq!(frame.body, serde_json::json!({ "text": "hello world" }));
        assert_eq!(frame.dropped_count, None);
    }

    #[test]
    fn progress_content_serializes_blocks_into_body() {
        let id = call_id();
        let blocks = vec![
            ContentBlock::Text {
                text: "line one".to_owned(),
            },
            ContentBlock::Text {
                text: "line two".to_owned(),
            },
        ];
        let expected_body = serde_json::to_value(&blocks).unwrap();
        let progress = ToolProgress::Content { blocks };
        let frame = progress_to_frame(progress, id.clone());
        assert_eq!(frame.tool_call_id, id);
        assert_eq!(frame.kind, "content");
        assert_eq!(frame.body, expected_body);
        assert_eq!(frame.dropped_count, None);
    }

    #[test]
    fn progress_custom_preserves_subkind_and_payload() {
        let id = call_id();
        let payload = serde_json::json!({ "cursor": 42, "partial": true });
        let progress = ToolProgress::Custom {
            subkind: "my_tool.cursor".to_owned(),
            payload: payload.clone(),
        };
        let frame = progress_to_frame(progress, id.clone());
        assert_eq!(frame.tool_call_id, id);
        assert_eq!(frame.kind, "my_tool.cursor");
        assert_eq!(frame.body, payload);
        assert_eq!(frame.dropped_count, None);
    }

    // ── edge cases ──────────────────────────────────────────────────

    #[test]
    fn progress_text_empty_string() {
        let id = call_id();
        let progress = ToolProgress::Text {
            text: String::new(),
        };
        let frame = progress_to_frame(progress, id);
        assert_eq!(frame.kind, "text");
        assert_eq!(frame.body, serde_json::json!({ "text": "" }));
    }

    #[test]
    fn progress_content_empty_blocks() {
        let id = call_id();
        let progress = ToolProgress::Content { blocks: vec![] };
        let frame = progress_to_frame(progress, id);
        assert_eq!(frame.kind, "content");
        assert_eq!(frame.body, serde_json::json!([]));
    }

    #[test]
    fn progress_custom_null_payload_and_empty_subkind() {
        let id = call_id();
        let progress = ToolProgress::Custom {
            subkind: String::new(),
            payload: Value::Null,
        };
        let frame = progress_to_frame(progress, id);
        assert_eq!(frame.kind, "");
        assert_eq!(frame.body, Value::Null);
    }

    // ── content block coverage ──────────────────────────────────────

    #[test]
    fn progress_content_with_image_block() {
        let id = call_id();
        let blocks = vec![ContentBlock::Image {
            mime_type: "image/png".to_owned(),
            data: "base64data".to_owned(),
            media_id: Some("img-1".to_owned()),
            filename: None,
            path: None,
            metadata: Default::default(),
        }];
        let expected_body = serde_json::to_value(&blocks).unwrap();
        let progress = ToolProgress::Content { blocks };
        let frame = progress_to_frame(progress, id);
        assert_eq!(frame.kind, "content");
        assert_eq!(frame.body, expected_body);
    }

    #[test]
    fn progress_content_with_resource_block() {
        let id = call_id();
        let blocks = vec![ContentBlock::Resource {
            uri: "file:///tmp/data.csv".to_owned(),
            mime_type: Some("text/csv".to_owned()),
            text: Some("a,b\n1,2".to_owned()),
        }];
        let expected_body = serde_json::to_value(&blocks).unwrap();
        let progress = ToolProgress::Content { blocks };
        let frame = progress_to_frame(progress, id);
        assert_eq!(frame.kind, "content");
        assert_eq!(frame.body, expected_body);
        assert_eq!(frame.body[0]["type"], "resource");
        assert_eq!(frame.body[0]["uri"], "file:///tmp/data.csv");
        assert_eq!(frame.body[0]["mime_type"], "text/csv");
        assert_eq!(frame.body[0]["text"], "a,b\n1,2");
    }

    // ── serde round-trips ───────────────────────────────────────────

    #[test]
    fn progress_frame_text_round_trips() {
        let progress = ToolProgress::Text {
            text: "round-trip".to_owned(),
        };
        let frame = progress_to_frame(progress, call_id());
        let json = serde_json::to_value(&frame).expect("serialize");
        let back: ToolCallProgressFrame = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, frame);
    }

    #[test]
    fn progress_frame_content_round_trips() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".to_owned(),
            },
            ContentBlock::Image {
                mime_type: "image/png".to_owned(),
                data: "abc".to_owned(),
                media_id: None,
                filename: None,
                path: None,
                metadata: Default::default(),
            },
        ];
        let progress = ToolProgress::Content { blocks };
        let frame = progress_to_frame(progress, call_id());
        let json = serde_json::to_value(&frame).expect("serialize");
        let back: ToolCallProgressFrame = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, frame);
    }

    #[test]
    fn progress_frame_custom_round_trips() {
        let progress = ToolProgress::Custom {
            subkind: "streaming.chunk".to_owned(),
            payload: serde_json::json!({ "offset": 1024, "data": [1, 2, 3] }),
        };
        let frame = progress_to_frame(progress, call_id());
        let json = serde_json::to_value(&frame).expect("serialize");
        let back: ToolCallProgressFrame = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, frame);
    }

    // ── wire notification shape ─────────────────────────────────────

    #[test]
    fn progress_notification_has_correct_wire_shape() {
        let id = call_id();
        let sid = SessionId::new("test-sess").expect("valid");
        let progress = ToolProgress::Text {
            text: "wire test".to_owned(),
        };
        let frame = progress_to_frame(progress, id.clone());
        // Build with typed params — same generic instantiation as production.
        let notif = JsonRpcNotification {
            jsonrpc: JsonRpcVersion,
            session_id: Some(sid.clone()),
            seq: None,
            method: Method::ToolCallProgress.as_wire_str().to_owned(),
            params: frame,
        };
        let json: Value = serde_json::from_str(&serde_json::to_string(&notif).unwrap()).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["method"], "tool_call_progress");
        assert_eq!(json["session_id"], sid.as_str());
        assert!(json.get("id").is_none(), "notifications must not have id");
        assert!(json.get("seq").is_none(), "None seq must be omitted");
        // params.kind
        assert_eq!(json["params"]["kind"], "text");
        // params.tool_call_id present as string
        assert_eq!(json["params"]["tool_call_id"], id.as_str());
        // params.body.text matches
        assert_eq!(json["params"]["body"]["text"], "wire test");
        // dropped_count absent when None
        assert!(
            json["params"].get("dropped_count").is_none(),
            "None dropped_count must be omitted"
        );
    }

    #[test]
    fn json_serialized_len_matches_to_string() {
        let v = serde_json::json!({"a": 1, "b": ["x", "y"], "c": {"d": true}});
        assert_eq!(
            json_serialized_len(&v).unwrap(),
            serde_json::to_string(&v).unwrap().len()
        );
    }

    #[test]
    fn system_notify_data_error_propagates_not_unsupported() {
        let outcome = ResponseOutcome::Error(JsonRpcError {
            code: -32601,
            message: "server not found".to_owned(),
            data: Some(serde_json::json!({"code": "tool_server_not_found"})),
        });
        assert!(system_notify_ack_from_outcome(outcome).is_err());
    }

    #[test]
    fn system_notify_ok_reply_maps_to_accepted() {
        let outcome = ResponseOutcome::Result(serde_json::json!({}));
        assert_eq!(
            system_notify_ack_from_outcome(outcome).unwrap(),
            SystemNotifyAck::Accepted
        );
    }

    #[test]
    fn system_notify_method_not_found_maps_to_forwarding_unsupported() {
        let outcome = ResponseOutcome::Error(JsonRpcError {
            code: -32601,
            message: "method not found".to_owned(),
            data: None,
        });
        assert_eq!(
            system_notify_ack_from_outcome(outcome).unwrap(),
            SystemNotifyAck::ForwardingUnsupported
        );
    }

    #[test]
    fn system_notify_other_error_propagates() {
        let outcome = ResponseOutcome::Error(JsonRpcError {
            code: -32602,
            message: "invalid params".to_owned(),
            data: None,
        });
        assert!(system_notify_ack_from_outcome(outcome).is_err());
    }
}
