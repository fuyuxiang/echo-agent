//! Session-actor side `StatusDispatcher` for MCP client events.
//!
//! Receives [`echo_agent_mcp::servers::McpClientEvent`]s emitted by:
//! - per-client transport-liveness watchers
//!   ([`echo_agent_mcp::liveness`]),
//! - the [`echo_agent_mcp::servers::EchoAgentClientHandler`] (server-pushed
//!   `tools/list_changed` and `resources/list_changed`),
//! - the `ensure_initialized` success/failure path,
//! - the session MCP config diff path (`UpdateMcpServers` / toggle).
//!
//! Coalesces events in a **50 ms tumbling window** keyed by
//! `(server_name, McpClientEventKind)`. Two events with the same key
//! collapse into the latest one — e.g. an MCP server bursting 100
//! `tools/list_changed` notifications inside 10 ms produces exactly
//! one ACP push.
//!
//! Each surviving entry is emitted as an ACP
//! [`agent_client_protocol::ExtNotification`] with method
//! `echo.agent/mcp/server_status` and the payload schema defined by
//! [`McpServerStatusPayload`].
//!
//! ## Doc-comment ↔ implementation contract
//!
//! - Coalescing window is exactly 50 ms, tumbling — events received
//!   during the window are buffered, then flushed on the next tick.
//! - Per `(server, kind)` collapse: the *latest* event wins (events
//!   inserted into a `HashMap` are overwritten by later inserts).
//! - `ConfigDiff` is fanned out per-server, **not** stored as a
//!   single event in the buffer.
//! - The bounded auto-restart task wires in: after a
//!   window flush, the dispatcher hands off each `TransportClosed` /
//!   `HandshakeFailed` key to
//!   [`crate::session::mcp_restart::maybe_schedule_restart`], which
//!   applies the stdio-only / shutting-down / configured-and-enabled
//!   guard rails before spawning
//!   [`crate::session::mcp_restart::auto_restart_stdio`]. The
//!   dispatcher itself stays single-purpose: coalesce + push.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol as acp;
use echo_agent_mcp::servers::{
    McpClientEvent, McpClientEventKind, McpServerName, McpState, mcp_server_name, mcp_transport_str,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::extensions::mcp::{MANAGED_GATEWAY_ENTRY_PREFIX, McpServerSource};

/// Tumbling-window coalescing period. See module doc.
pub(crate) const COALESCE_WINDOW: Duration = Duration::from_millis(50);

/// Method name for the ACP push.
pub const SERVER_STATUS_METHOD: &str = "echo.agent/mcp/server_status";

/// JSON payload pushed over ACP. Fields written in camelCase per ACP
/// convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatusPayload {
    /// Owning session id.
    pub session_id: String,
    /// MCP server name (`managed_gateway:linear`, `github`, ...).
    pub name: String,
    /// `managed` for gateway catalog ids (`managed_gateway:*`), else `local`.
    pub source: McpServerSource,
    /// Current status — see [`McpServerStatus`].
    pub status: McpServerStatus,
    /// What drove the status change. See [`McpServerStatusReason`].
    pub reason: McpServerStatusReason,
    /// Optional human-readable detail. Surfaces the full handshake /
    /// transport error reason to the UI verbatim — no sanitization or
    /// truncation — so failures are easy to debug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Reserved for future use; always `null` today; may fill
    /// this with the post-restart tool list so the client can
    /// re-render without a follow-up `mcp/list` round-trip.
    pub tools: Option<serde_json::Value>,
}

/// Status enum surfaced to the wire. Lowercase serialization to
/// match the existing pager `McpSessionStatus` family.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpServerStatus {
    /// Client is in [`echo_agent_mcp::servers::ClientStateKind::Ready`]
    /// and the transport is healthy.
    Ready,
    /// Per-server handshake is in flight, or a restart is being
    /// debounced.
    Initializing,
    /// Transport closed, handshake failed, or the server is
    /// disabled/unconfigured.
    Unavailable,
    /// OAuth required but not yet acquired.
    NeedsAuth,
}

/// Reason a status delta was emitted. Lowercase + snake_case
/// serialization to keep the wire schema stable.
///
/// `RestartSucceeded` / `RestartFailed` are reserved for the
/// auto-restart path. `Initialized` is emitted for the first-time
/// `Ready` transition out of `ensure_initialized` — distinguishing
/// a brand-new handshake from a successful re-handshake (`Ready →
/// restart_succeeded` was the wire before).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatusReason {
    TransportClosed,
    HandshakeFailed,
    ConfigAdded,
    ConfigRemoved,
    ConfigChanged,
    Disabled,
    AuthExpired,
    /// First-time successful handshake (a new server transitioned
    /// from `Initializing` → `Ready`). Every
    /// `McpClientEvent::Ready` maps to this reason.
    Initialized,
    /// A watcher fired `TransportClosed`, the
    /// auto-restart path re-handshook, and the new handshake
    /// succeeded.
    RestartSucceeded,
    /// The auto-restart path exhausted retries.
    RestartFailed,
    /// Old leaders still emit this after reactive reauth. Not produced anymore.
    ManagedTokenRefreshed,
}

/// Build [`McpServerSource`] from a server name. Live managed connectors are
/// gateway catalog rows (`managed_gateway:*`); everything else is local.
pub(crate) fn classify_source(name: &str) -> McpServerSource {
    if name.starts_with(MANAGED_GATEWAY_ENTRY_PREFIX) {
        McpServerSource::Managed
    } else {
        McpServerSource::Local
    }
}

/// State for the dispatcher's "intentional teardown" tracking.
///
/// The set carries servers whose `Arc<McpClient>` was intentionally
/// dropped by the session (config diff removed it, or
/// `ToggleMcpServer enabled=false`). The auto-restart task
/// consults this set so a `TransportClosed` that arrives *because*
/// the dispatcher's own `kill_on_drop` killed the child does NOT
/// resurrect a server the user just deleted.
///
/// ## Producers (mark)
///
/// `flush_window` marks the set when it observes a
/// `ConfigRemoved` event. `ConfigRemoved` is emitted by the
/// session actor's `UpdateMcpServers` / `ToggleMcpServer
/// enabled=false` arms (via `client_event_tx.send(ConfigDiff)`
/// pre-`update_configs_diff`). `TransportClosed` is **not** a
/// producer — that event is exactly what auto-restart is meant
/// to react to, so marking on it would create the C1 deadlock
/// where every crash is also self-classified as a teardown.
///
/// ## Consumers (read)
///
/// - `crate::session::mcp_restart::maybe_schedule_restart`:
///   skip auto-restart for servers in the set.
/// - Dispatcher de-dup of follow-up events on a
///   server already declared unavailable on the wire.
///
/// ## Clearing
///
/// Entries are cleared **only** by an `McpClientEventKind::Ready`
/// observation in `flush_window` (re-handshake succeeded, server is
/// back). There is **no time-based expiry** — a config-removed
/// server stays in the set until something re-emits `Ready`, which
/// for a permanently-removed server never happens. That's the
/// intended behavior: a removed server is gone.
///
/// (An earlier draft of this struct documented a 5 s grace timer;
/// that timer was never wired. The doc has been updated to match
/// the actual semantics.)
#[derive(Default)]
pub(crate) struct ShutdownState {
    shutting_down: HashSet<McpServerName>,
    /// Servers with an `auto_restart_stdio` task currently in flight.
    ///
    /// Dedup set: `maybe_schedule_restart` claims a
    /// server here before spawning, so a second `TransportClosed` /
    /// `HandshakeFailed` arriving while the first respawn is mid-backoff
    /// or mid-handshake is short-circuited instead of spawning a
    /// duplicate task (which would race on `start_mcp_server` and
    /// `owned_clients.insert`, orphaning an stdio child). Released by
    /// the spawned task's RAII guard on every exit path. Lives next to
    /// `shutting_down` so both share the one `SharedShutdownState` lock.
    in_flight_restart: HashSet<McpServerName>,
}

impl ShutdownState {
    pub(crate) fn mark(&mut self, name: McpServerName) {
        self.shutting_down.insert(name);
    }
    /// Used by the auto-restart task: when `true`, skip respawn
    /// because the session is intentionally tearing the client
    /// down (config diff / toggle-off).
    pub(crate) fn is_shutting_down(&self, name: &str) -> bool {
        self.shutting_down.contains(name)
    }
    pub(crate) fn forget(&mut self, name: &str) {
        self.shutting_down.remove(name);
    }

    /// Atomically claim the in-flight restart slot for `name`. Returns
    /// `true` if newly claimed, `false` if a restart is already in
    /// flight. See `in_flight_restart`.
    pub(crate) fn begin_restart(&mut self, name: McpServerName) -> bool {
        self.in_flight_restart.insert(name)
    }
    /// Release the in-flight restart claim taken by [`Self::begin_restart`].
    pub(crate) fn end_restart(&mut self, name: &str) {
        self.in_flight_restart.remove(name);
    }
}

/// Shared reference to a [`ShutdownState`] used by both the
/// dispatcher loop (writer: `mark` / `forget` from `flush_window`)
/// and the auto-restart actions (reader: `is_shutting_down` from
/// `mcp_restart::auto_restart_stdio`).
///
/// `std::sync::Mutex` is sufficient because every caller acquires
/// the lock synchronously and holds it only for the duration of a
/// `HashSet` insert / lookup. The dispatcher's per-flush update path
/// also takes the lock under `flush_window`, which itself runs
/// synchronously.
pub(crate) type SharedShutdownState = Arc<std::sync::Mutex<ShutdownState>>;

/// Construct a fresh shared `ShutdownState`. Convenience helper so
/// the session actor and the unit tests don't need to repeat the
/// `Arc<std::sync::Mutex<_>>` boilerplate.
pub(crate) fn new_shutdown_state() -> SharedShutdownState {
    Arc::new(std::sync::Mutex::new(ShutdownState::default()))
}

/// One flushed coalesce window. `buf` is last-write-wins per
/// `(server, kind)` — the right dedup for wire pushes — but that
/// collapse could discard the `TransportClosed` id that matches the
/// registered client when several closes land in one window, so
/// `closed` accumulates every close identity per server.
#[derive(Default)]
pub(crate) struct CoalescedWindow {
    /// Last-write-wins per `(server, kind)` — wire-push dedup.
    pub buf: HashMap<(McpServerName, McpClientEventKind), McpClientEvent>,
    /// All `TransportClosed` client identities per server seen in the
    /// window.
    pub closed: HashMap<McpServerName, HashSet<u64>>,
    pub completes: Vec<(McpServerName, String)>,
}

/// Coalesce the buffered events for one window flush.
///
/// Public so unit tests can exercise the coalescing logic without
/// spinning up the full dispatcher task.
///
/// Algorithm:
/// 1. Receive the first event (blocks). If the channel closes,
///    returns `Ok(None)` to signal task exit.
/// 2. Insert it into a fresh [`CoalescedWindow`].
/// 3. Read additional events with `timeout_at(deadline, recv)`
///    until the 50 ms window closes. Each insert overwrites
///    same-key buffer entries — last-write-wins — while
///    `TransportClosed` identities accumulate in
///    [`CoalescedWindow::closed`].
/// 4. Fan out `ConfigDiff` into per-server `ConfigAdded` /
///    `ConfigRemoved` keys at insertion time.
///
/// Returns the coalesced window.
pub(crate) async fn collect_window(
    rx: &mut UnboundedReceiver<McpClientEvent>,
    window: Duration,
) -> Option<CoalescedWindow> {
    let first = rx.recv().await?;
    let mut win = CoalescedWindow::default();
    insert_event(&mut win, first);

    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) => insert_event(&mut win, ev),
            // Channel closed: stop collecting; the dispatcher loop
            // will see `None` on its next `rx.recv()` and exit.
            Ok(None) => break,
            // Window deadline reached.
            Err(_) => break,
        }
    }

    Some(win)
}

/// Insert one event into the coalesce window. Fans `ConfigDiff`
/// out into per-server `ConfigAdded` / `ConfigRemoved` events
/// (using the dedicated `McpClientEvent::ConfigAdded` /
/// `ConfigRemoved` variants) so the rest
/// of the pipeline only has to handle per-server entries.
/// `TransportClosed` identities additionally accumulate in
/// [`CoalescedWindow::closed`] (see that field's doc).
fn insert_event(win: &mut CoalescedWindow, ev: McpClientEvent) {
    match ev {
        McpClientEvent::ConfigDiff { added, removed } => {
            for name in added {
                win.buf.insert(
                    (name.clone(), McpClientEventKind::ConfigAdded),
                    McpClientEvent::ConfigAdded { server: name },
                );
            }
            for name in removed {
                win.buf.insert(
                    (name.clone(), McpClientEventKind::ConfigRemoved),
                    McpClientEvent::ConfigRemoved { server: name },
                );
            }
        }
        McpClientEvent::ElicitationComplete {
            server,
            elicitation_id,
        } => {
            win.completes.push((server, elicitation_id));
        }
        ev => {
            if let McpClientEvent::TransportClosed { server, client_id } = &ev {
                win.closed
                    .entry(server.clone())
                    .or_default()
                    .insert(*client_id);
            }
            let kind = kind_of(&ev);
            // server_name() returns None only for ConfigDiff which
            // is handled above; the unwrap_or here exists purely
            // for defense — if we ever add a payload-less variant
            // we'll get a deterministic key string rather than a
            // panic.
            let server = ev.server_name().unwrap_or("").to_string();
            win.buf.insert((server, kind), ev);
        }
    }
}

/// Discriminant for an event (mirrors
/// [`echo_agent_mcp::servers::McpClientEventKind`]). Used as the
/// second half of the coalescing key.
///
/// `ConfigDiff` is fanned out by [`insert_event`] before ever
/// reaching this function, so it's truly unreachable here. The
/// explicit panic replaces the previous silent
/// `→ ConfigAdded` fallback with an explicit panic — a future
/// caller that hands a `ConfigDiff` in directly will see the
/// failure immediately instead of producing wrong coalescing keys.
fn kind_of(ev: &McpClientEvent) -> McpClientEventKind {
    match ev {
        McpClientEvent::TransportClosed { .. } => McpClientEventKind::TransportClosed,
        McpClientEvent::HandshakeFailed { .. } => McpClientEventKind::HandshakeFailed,
        McpClientEvent::ToolsChanged { .. } => McpClientEventKind::ToolsChanged,
        McpClientEvent::ResourcesChanged { .. } => McpClientEventKind::ResourcesChanged,
        McpClientEvent::ElicitationComplete { .. } => McpClientEventKind::ElicitationComplete,
        McpClientEvent::Ready { .. } => McpClientEventKind::Ready,
        McpClientEvent::ConfigAdded { .. } => McpClientEventKind::ConfigAdded,
        McpClientEvent::ConfigRemoved { .. } => McpClientEventKind::ConfigRemoved,
        McpClientEvent::ConfigDiff { .. } => {
            unreachable!(
                "ConfigDiff is fanned out into ConfigAdded/ConfigRemoved by insert_event before kind_of is called"
            )
        }
    }
}

/// Project an [`McpClientEvent`] + its coalescing key into a
/// wire-ready [`McpServerStatusPayload`].
///
/// The `HandshakeFailed` `reason` is passed through verbatim as the
/// `detail` field (full error, no sanitization) to ease debugging.
pub(crate) fn build_payload(
    session_id: &str,
    key: &(McpServerName, McpClientEventKind),
    event: &McpClientEvent,
) -> McpServerStatusPayload {
    let (server, kind) = key;
    let source = classify_source(server);

    let (status, reason, detail) = match (kind, event) {
        (McpClientEventKind::TransportClosed, _) => (
            McpServerStatus::Unavailable,
            McpServerStatusReason::TransportClosed,
            None,
        ),
        (McpClientEventKind::HandshakeFailed, McpClientEvent::HandshakeFailed { reason, .. }) => {
            let detail = reason.clone();
            (
                McpServerStatus::Unavailable,
                McpServerStatusReason::HandshakeFailed,
                Some(detail),
            )
        }
        (McpClientEventKind::HandshakeFailed, _) => (
            McpServerStatus::Unavailable,
            McpServerStatusReason::HandshakeFailed,
            None,
        ),
        (McpClientEventKind::ToolsChanged, _) => (
            McpServerStatus::Ready,
            McpServerStatusReason::ConfigChanged,
            None,
        ),
        // Diverted into `CoalescedWindow::completes` by `insert_event`,
        // so this kind never appears in `buf`.
        (McpClientEventKind::ElicitationComplete, _) => {
            unreachable!("ElicitationComplete is diverted into win.completes by insert_event")
        }
        (McpClientEventKind::ResourcesChanged, _) => (
            McpServerStatus::Ready,
            McpServerStatusReason::ConfigChanged,
            None,
        ),
        // `Ready` is only emitted from the first-time
        // `ensure_initialized` path. Map it to `Initialized`, NOT
        // `RestartSucceeded` (which is reserved for the
        // auto-restart code path).
        (McpClientEventKind::Ready, _) => (
            McpServerStatus::Ready,
            McpServerStatusReason::Initialized,
            None,
        ),
        (McpClientEventKind::ConfigAdded, _) => (
            McpServerStatus::Initializing,
            McpServerStatusReason::ConfigAdded,
            None,
        ),
        (McpClientEventKind::ConfigRemoved, _) => (
            McpServerStatus::Unavailable,
            McpServerStatusReason::ConfigRemoved,
            None,
        ),
    };

    McpServerStatusPayload {
        session_id: session_id.to_string(),
        name: server.clone(),
        source,
        status,
        reason,
        detail,
        tools: None,
    }
}

/// Per-flush side effects:
/// - update `shutting_down` for `TransportClosed` /
///   `ConfigRemoved` keys,
/// - emit one ACP `echo.agent/mcp/server_status` push per surviving
///   buffer entry, via the provided gateway.
///
/// `gateway` is a [`echo_agent_acp::AcpAgentGatewaySender`] (forwarded
/// fire-and-forget). Failures are logged and dropped — the
/// dispatcher must not block the session actor.
pub(crate) fn flush_window(
    session_id: &str,
    buf: HashMap<(McpServerName, McpClientEventKind), McpClientEvent>,
    shutdown: &SharedShutdownState,
    gateway: &echo_agent_acp::AcpAgentGatewaySender,
) {
    // Recover from poisoning rather than cascade-panicking: `flush_window`
    // now does non-trivial work under this lock, and a single panic while
    // holding it would otherwise turn every future restart-task check and
    // dispatcher window into a panic. The `HashSet` state remains coherent
    // across a panic (no half-updated invariant), so `into_inner()` is safe.
    let mut shutdown_guard = shutdown.lock().unwrap_or_else(|e| e.into_inner());
    for (key, event) in buf {
        let (server, kind) = &key;
        // ONLY `ConfigRemoved` marks `shutting_down`.
        //
        // Pre-fix, `TransportClosed` also marked the set, but
        // `run_dispatcher` then immediately fed every
        // `TransportClosed` key into `maybe_schedule_restart`, whose
        // first guard rail short-circuits on `is_in_shutting_down`.
        // Net effect: every stdio crash was self-classified as an
        // intentional shutdown and auto-restart never fired in
        // production. The mark must signal user intent (config
        // removed / toggled off), not transport death.
        //
        // `Ready` clears the mark — a server that just successfully
        // (re-)handshook is back; future events on this server
        // should be processed normally.
        match kind {
            McpClientEventKind::ConfigRemoved => {
                shutdown_guard.mark(server.clone());
            }
            McpClientEventKind::Ready => {
                shutdown_guard.forget(server);
            }
            _ => {}
        }

        let payload = build_payload(session_id, &key, &event);
        let raw = match serde_json::value::to_raw_value(&payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %server, error = %e, "failed to serialize mcp/server_status");
                continue;
            }
        };
        gateway
            .forward_fire_and_forget(acp::ExtNotification::new(SERVER_STATUS_METHOD, raw.into()));
    }
}

fn flush_elicitation_completes(
    session_id: &str,
    completes: Vec<(McpServerName, String)>,
    gateway: &echo_agent_acp::AcpAgentGatewaySender,
) {
    for (server, elicitation_id) in completes {
        let payload = echo_agent_tools::mcp_elicitation::McpElicitCompletePayload {
            session_id: session_id.to_string(),
            elicitation_id,
            server_name: Some(server.clone()),
        };
        match serde_json::value::to_raw_value(&payload) {
            Ok(raw) => {
                gateway.forward_fire_and_forget(acp::ExtNotification::new(
                    echo_agent_mcp::wire::MCP_ELICIT_COMPLETE,
                    raw.into(),
                ));
            }
            Err(e) => {
                tracing::warn!(
                    server = %server,
                    error = %e,
                    "failed to serialize mcp/elicit_complete"
                );
            }
        }
    }
}

/// A server with one or more `TransportClosed` ids in the window —
/// produced by [`collect_close_candidates`] and consumed by
/// [`drop_dead_clients`], which decides per-candidate whether the
/// registered client is actually dead (id match) or a live replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadClient {
    pub server: McpServerName,
    /// Every `TransportClosed` client id observed for this server in
    /// the window. Eviction fires when the registered client's id is
    /// in this set.
    pub closed: HashSet<u64>,
}

/// Walk the coalesce window and return the close *candidates* — servers
/// whose owned `Arc<McpClient>` may need to be torn down from
/// [`McpState::owned_clients`]. [`drop_dead_clients`] then evicts only
/// the ones whose registered client id actually matches a close.
///
/// Only stdio `TransportClosed` participates:
///
/// - HTTP `TransportClosed` (`http_servers`) is **excluded**: those
///   clients are recovered in place by [`collect_http_transport_closed`]
///   so their tools stay valid.
/// - `ConfigRemoved` is **excluded**: every `ConfigDiff` producer calls
///   `McpState::update_configs_diff` first, which removes the old client
///   synchronously *before* the event is emitted — so an entry still
///   present at flush time can only be a freshly-handshook replacement
///   from a remove+re-add.
pub(crate) fn collect_close_candidates(
    win: &CoalescedWindow,
    http_servers: &HashSet<McpServerName>,
) -> Vec<DeadClient> {
    win.closed
        .iter()
        .filter(|(server, _)| !http_servers.contains(*server))
        .map(|(server, closed)| DeadClient {
            server: server.clone(),
            closed: closed.clone(),
        })
        .collect()
}

/// Names eligible for in-place HTTP recovery: enabled HTTP/SSE config entries.
///
/// MUST match the recovery gate (`SessionActor::is_http_server_configured`).
/// If the two predicates diverge, a disabled HTTP server still present in
/// `configs` is kept here (not evicted) yet rejected by the gate (not
/// recovered) — orphaning a dead client in `owned_clients`.
pub(crate) fn recoverable_http_servers(
    configs: &[acp::McpServer],
    disabled: &HashSet<String>,
) -> HashSet<McpServerName> {
    configs
        .iter()
        .filter(|c| {
            matches!(
                c,
                acp::McpServer::Http(acp::McpServerHttp { .. })
                    | acp::McpServer::Sse(acp::McpServerSse { .. })
            )
        })
        .map(|c| mcp_server_name(c).to_string())
        .filter(|name| !disabled.contains(name))
        .collect()
}

/// Server names with a `TransportClosed` event whose transport is HTTP —
/// the candidates for in-place HTTP recovery (kept, not evicted, by
/// [`collect_close_candidates`]).
pub(crate) fn collect_http_transport_closed(
    buf: &HashMap<(McpServerName, McpClientEventKind), McpClientEvent>,
    http_servers: &HashSet<McpServerName>,
) -> Vec<McpServerName> {
    buf.keys()
        .filter(|(server, kind)| {
            matches!(kind, McpClientEventKind::TransportClosed) && http_servers.contains(server)
        })
        .map(|(server, _)| server.clone())
        .collect()
}

/// Drop dead clients from `McpState::owned_clients`. Holds the
/// `McpState` lock for the duration of the iteration; removing a
/// server that isn't registered is a no-op.
///
/// Eviction is keyed by *client identity*: a close for id N must not
/// evict a replacement (id M ≠ N) registered under the same name
/// while the event sat in the coalesce window.
///
/// Returns the servers whose `TransportClosed` was *stale* (a current
/// client exists and no closed id matches it). The caller must strip
/// those buffered entries so a stale event is fully inert: no
/// `unavailable` push, no disconnect span, no spurious restart.
pub(crate) async fn drop_dead_clients(
    mcp_state: &Arc<TokioMutex<McpState>>,
    dead: &[DeadClient],
) -> Vec<McpServerName> {
    let mut stale = Vec::new();
    if dead.is_empty() {
        return stale;
    }
    let mut state = mcp_state.lock().await;
    for d in dead {
        let Some(current) = state.owned_clients.get(&d.server) else {
            // Nothing registered: nothing to evict, and the death
            // status is still accurate — don't mark stale.
            continue;
        };
        if d.closed.contains(&current.client_id()) {
            state.owned_clients.remove(&d.server);
            tracing::info!(
                server = %d.server,
                closed_ids = ?d.closed,
                "mcp status dispatcher dropped dead client from owned_clients",
            );
        } else {
            stale.push(d.server.clone());
            tracing::info!(
                server = %d.server,
                closed_ids = ?d.closed,
                current_client_id = current.client_id(),
                "mcp status dispatcher: stale TransportClosed for a replaced client, keeping current client",
            );
        }
    }
    stale
}

/// Drive the dispatcher loop. Runs until `rx` is closed.
///
/// Spawned via `tokio::task::spawn_local` from the session actor's
/// LocalSet (the gateway is `!Send`, so this MUST run on a
/// LocalSet).
///
/// Per-window pipeline:
/// 1. `collect_window` — block on `rx.recv()`, then accumulate up
///    to 50 ms of events (last-write-wins per `(server, kind)`).
/// 2. `drop_dead_clients` — remove `TransportClosed` entries from
///    [`McpState::owned_clients`] BEFORE pushing status notifications,
///    gated on client identity (see [`collect_close_candidates`]).
///    Stale `TransportClosed` keys are stripped from the window so they
///    push no status, emit no disconnect span, and schedule no restart.
/// 3. `flush_window` — emit ACP `echo.agent/mcp/server_status` per
///    surviving entry.
/// 4. `maybe_schedule_restart` — for every
///    `TransportClosed` / `HandshakeFailed` key, the
///    [`crate::session::mcp_restart`] gate decides whether to spawn
///    an `auto_restart_stdio` task. Skipped entirely when
///    `restart_actions` is `None` (e.g. `mcp.auto_restart=false`).
pub(crate) async fn run_dispatcher(
    session_id: String,
    mut rx: UnboundedReceiver<McpClientEvent>,
    gateway: echo_agent_acp::AcpAgentGatewaySender,
    mcp_state: Arc<TokioMutex<McpState>>,
    shutdown: SharedShutdownState,
    restart_actions: Option<Rc<dyn crate::session::mcp_restart::RestartActions>>,
    cwd: std::path::PathBuf,
) {
    // Cancellation source for spawned `auto_restart_stdio` tasks. The
    // dispatcher exiting (channel closed) means the session is shutting
    // down, so we cancel — any in-flight backoff sleep aborts promptly
    // instead of running for up to 21s and pushing status through a
    // gateway that is already tearing down. Tasks select on this token
    // during their backoff (see `mcp_restart::auto_restart_stdio`).
    let restart_cancel = tokio_util::sync::CancellationToken::new();
    loop {
        let Some(mut win) = collect_window(&mut rx, COALESCE_WINDOW).await else {
            tracing::debug!(
                session_id = %session_id,
                "mcp status dispatcher channel closed; exiting",
            );
            break;
        };
        let completes = std::mem::take(&mut win.completes);
        // Completes are independent fire-and-forget notifications, so they
        // flush here regardless of whether any status entries survive below.
        flush_elicitation_completes(&session_id, completes, &gateway);
        if win.buf.is_empty() {
            continue;
        }
        let has_transport_closed = win
            .buf
            .keys()
            .any(|(_, k)| matches!(k, McpClientEventKind::TransportClosed));
        let has_config_removed = win
            .buf
            .keys()
            .any(|(_, k)| matches!(k, McpClientEventKind::ConfigRemoved));

        // Classify each configured server's transport (single lock).
        // `http_servers` decides recover-in-place vs evict; `transport_map`
        // feeds the disconnect telemetry below. `recoverable_http_servers`
        // excludes disabled names so it stays in lockstep with the
        // recovery gate `is_http_server_configured` — otherwise a disabled
        // HTTP server would be neither evicted nor recovered.
        let (http_servers, transport_map): (
            HashSet<McpServerName>,
            HashMap<McpServerName, &'static str>,
        ) = if has_transport_closed || has_config_removed {
            let disabled = crate::util::config::disabled_mcp_server_names(&cwd);
            let state = mcp_state.lock().await;
            let http = recoverable_http_servers(&state.configs, &disabled);
            let tmap = state
                .configs
                .iter()
                .map(|c| (mcp_server_name(c).to_string(), mcp_transport_str(c)))
                .collect();
            (http, tmap)
        } else {
            (HashSet::new(), HashMap::new())
        };

        // Evict dead stdio clients, gated on client identity (see
        // [`collect_close_candidates`]). HTTP `TransportClosed` clients
        // are KEPT for in-place recovery; `ConfigRemoved` is not evicted
        // here (the config diff already dropped the old client).
        let dead = collect_close_candidates(&win, &http_servers);
        let stale = drop_dead_clients(&mcp_state, &dead).await;
        // Strip stale closes BEFORE the restart-key capture, the
        // disconnect telemetry, and the status flush below, so the
        // healthy replacement is neither reported unavailable nor
        // respawned.
        for server in &stale {
            win.buf
                .remove(&(server.clone(), McpClientEventKind::TransportClosed));
        }
        let buf = win.buf;
        if buf.is_empty() {
            continue;
        }
        // Capture restart/recovery candidates BEFORE flush_window
        // consumes `buf` so we don't need to clone events.
        //
        // stdio respawn: `TransportClosed` (non-HTTP) + `HandshakeFailed`.
        let restart_keys: Vec<(McpServerName, McpClientEventKind)> = if restart_actions.is_some() {
            buf.keys()
                .filter(|(server, k)| match k {
                    McpClientEventKind::TransportClosed => !http_servers.contains(server),
                    McpClientEventKind::HandshakeFailed => true,
                    _ => false,
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        // in-place HTTP recovery: `TransportClosed` for HTTP servers.
        let http_recover: Vec<McpServerName> = if restart_actions.is_some() {
            collect_http_transport_closed(&buf, &http_servers)
        } else {
            Vec::new()
        };

        // Runtime disconnect spans (status=disconnected), enriched with
        // transport + scope from the live config — data flush_window can't
        // reach. Emitted before flush_window consumes `buf`.
        if has_transport_closed {
            for (server, kind) in buf.keys() {
                if matches!(kind, McpClientEventKind::TransportClosed) {
                    crate::session::telemetry::emit_mcp_connection_span(
                        "disconnected",
                        server.as_str(),
                        transport_map
                            .get(server.as_str())
                            .copied()
                            .unwrap_or("unknown"),
                        crate::util::config::mcp_server_scope(server, &cwd),
                        None,
                        None,
                        None,
                    );
                }
            }
        }
        flush_window(&session_id, buf, &shutdown, &gateway);
        if let Some(actions) = restart_actions.as_ref() {
            for (server, kind) in restart_keys {
                let _ = crate::session::mcp_restart::maybe_schedule_restart(
                    Rc::clone(actions),
                    session_id.clone(),
                    server,
                    kind,
                    restart_cancel.clone(),
                )
                .await;
            }
            for server in http_recover {
                let _ = crate::session::mcp_restart::maybe_schedule_http_recovery(
                    Rc::clone(actions),
                    server,
                    restart_cancel.clone(),
                )
                .await;
            }
        }
    }
    // Channel closed → session shutting down. Cancel any in-flight
    // auto-restart backoff sleeps so they don't outlive the dispatcher.
    restart_cancel.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    /// Contract: 100 ToolsChanged events for the same server within
    /// 10 ms coalesce into exactly one buffer entry.
    #[tokio::test(start_paused = true)]
    async fn coalesce_within_50ms_window() {
        let (tx, mut rx) = unbounded_channel::<McpClientEvent>();
        for _ in 0..100 {
            tx.send(McpClientEvent::ToolsChanged {
                server: "github".to_string(),
            })
            .unwrap();
        }
        // Drop the sender so collect_window terminates promptly
        // once the buffered events are drained — the window
        // deadline is the backstop. Under `start_paused = true`
        // time only advances on `tokio::time::advance` or when a
        // task awaits a timer.
        drop(tx);

        let win = collect_window(&mut rx, COALESCE_WINDOW)
            .await
            .expect("at least one event");
        assert_eq!(win.buf.len(), 1, "100 ToolsChanged collapse to one");
        let key = ("github".to_string(), McpClientEventKind::ToolsChanged);
        assert!(win.buf.contains_key(&key));
    }

    #[tokio::test(start_paused = true)]
    async fn elicitation_completes_accumulate_in_window() {
        let (tx, mut rx) = unbounded_channel::<McpClientEvent>();
        tx.send(McpClientEvent::ElicitationComplete {
            server: "github".to_string(),
            elicitation_id: "a".to_string(),
        })
        .unwrap();
        tx.send(McpClientEvent::ElicitationComplete {
            server: "github".to_string(),
            elicitation_id: "b".to_string(),
        })
        .unwrap();
        drop(tx);

        let win = collect_window(&mut rx, COALESCE_WINDOW)
            .await
            .expect("events arrived");
        assert!(win.buf.is_empty());
        assert_eq!(
            win.completes,
            vec![
                ("github".to_string(), "a".to_string()),
                ("github".to_string(), "b".to_string()),
            ]
        );
    }

    /// End-to-end: an `ElicitationComplete`-only window has an empty
    /// status buffer (`win.buf`), and the complete notification must
    /// still be forwarded to the client.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn run_dispatcher_forwards_completes_when_buf_is_empty() {
        use echo_agent_mcp::servers::McpState;

        let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
        let shutdown = new_shutdown_state();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (gw_tx, mut gw_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = echo_agent_acp::AcpAgentGatewaySender::new(gw_tx);

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let dispatcher = tokio::task::spawn_local(run_dispatcher(
                    "sess-1".to_string(),
                    rx,
                    gateway,
                    mcp_state,
                    shutdown,
                    None,
                    std::path::PathBuf::from("."),
                ));

                tx.send(McpClientEvent::ElicitationComplete {
                    server: "github".to_string(),
                    elicitation_id: "e-1".to_string(),
                })
                .unwrap();

                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_millis(60)).await;
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }

                let msg = gw_rx
                    .try_recv()
                    .expect("complete must be forwarded even with an empty status buffer");
                let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
                    panic!("expected ExtNotification");
                };
                assert_eq!(
                    args.request.method.as_ref(),
                    echo_agent_mcp::wire::MCP_ELICIT_COMPLETE
                );
                let v: serde_json::Value = serde_json::from_str(args.request.params.get()).unwrap();
                assert_eq!(v["sessionId"], "sess-1");
                assert_eq!(v["elicitationId"], "e-1");
                assert_eq!(v["serverName"], "github");

                dispatcher.abort();
            })
            .await;
    }

    /// Contract: events for different servers don't collapse,
    /// and events of different kinds for the same server also
    /// don't collapse.
    #[tokio::test(start_paused = true)]
    async fn coalesce_keys_distinguish_server_and_kind() {
        let (tx, mut rx) = unbounded_channel::<McpClientEvent>();
        tx.send(McpClientEvent::ToolsChanged {
            server: "github".to_string(),
        })
        .unwrap();
        tx.send(McpClientEvent::ToolsChanged {
            server: "linear".to_string(),
        })
        .unwrap();
        tx.send(McpClientEvent::Ready {
            server: "github".to_string(),
        })
        .unwrap();
        drop(tx);

        let win = collect_window(&mut rx, COALESCE_WINDOW)
            .await
            .expect("events arrived");
        assert_eq!(win.buf.len(), 3);
    }

    /// Contract: `ConfigDiff` is fanned out per-server, and the
    /// post-fan-out values are the dedicated
    /// `McpClientEvent::ConfigAdded` / `ConfigRemoved` variants —
    /// NOT the fake `Ready` / `TransportClosed` placeholders the
    /// earlier draft stored.
    #[tokio::test(start_paused = true)]
    async fn config_diff_fans_out_per_server() {
        let (tx, mut rx) = unbounded_channel::<McpClientEvent>();
        tx.send(McpClientEvent::ConfigDiff {
            added: vec!["new1".to_string(), "new2".to_string()],
            removed: vec!["old".to_string()],
        })
        .unwrap();
        drop(tx);

        let win = collect_window(&mut rx, COALESCE_WINDOW)
            .await
            .expect("events arrived");
        assert_eq!(win.buf.len(), 3);
        let added_key = ("new1".to_string(), McpClientEventKind::ConfigAdded);
        assert!(win.buf.contains_key(&added_key));
        match win.buf.get(&added_key).unwrap() {
            McpClientEvent::ConfigAdded { server } => assert_eq!(server, "new1"),
            other => panic!("expected ConfigAdded payload, got {other:?}"),
        }
        let removed_key = ("old".to_string(), McpClientEventKind::ConfigRemoved);
        assert!(win.buf.contains_key(&removed_key));
        match win.buf.get(&removed_key).unwrap() {
            McpClientEvent::ConfigRemoved { server } => assert_eq!(server, "old"),
            other => panic!("expected ConfigRemoved payload, got {other:?}"),
        }
    }

    /// End-to-end-ish: when the live-config-reload path
    /// runs `update_configs_diff` and emits `McpClientEvent::ConfigDiff`,
    /// the dispatcher must surface ONE `mcp/server_status` payload per
    /// added server with `reason: config_added` AND one per removed
    /// server with `reason: config_removed`. Builds the per-key
    /// payload directly using the same `build_payload` the live
    /// `flush_window` calls, so the assertion locks the exact wire
    /// shape `app.rs`'s `ProjectMcpServersChanged` arm produces
    /// downstream.
    #[test]
    fn project_config_change_emits_per_server_status_for_added_and_removed() {
        let added_key = ("server_x".to_string(), McpClientEventKind::ConfigAdded);
        let added_ev = McpClientEvent::ConfigAdded {
            server: "server_x".to_string(),
        };
        let added_payload = build_payload("sess-pr6", &added_key, &added_ev);
        assert_eq!(added_payload.name, "server_x");
        assert_eq!(added_payload.status, McpServerStatus::Initializing);
        assert_eq!(added_payload.reason, McpServerStatusReason::ConfigAdded);
        let added_json = serde_json::to_value(&added_payload).unwrap();
        assert_eq!(added_json["reason"], "config_added");

        let removed_key = ("server_y".to_string(), McpClientEventKind::ConfigRemoved);
        let removed_ev = McpClientEvent::ConfigRemoved {
            server: "server_y".to_string(),
        };
        let removed_payload = build_payload("sess-pr6", &removed_key, &removed_ev);
        assert_eq!(removed_payload.name, "server_y");
        assert_eq!(removed_payload.status, McpServerStatus::Unavailable);
        assert_eq!(removed_payload.reason, McpServerStatusReason::ConfigRemoved);
        let removed_json = serde_json::to_value(&removed_payload).unwrap();
        assert_eq!(removed_json["reason"], "config_removed");
    }

    /// Contract: payload status/reason mapping for TransportClosed.
    #[test]
    fn payload_maps_transport_closed_to_unavailable() {
        let key = ("linear".to_string(), McpClientEventKind::TransportClosed);
        let ev = McpClientEvent::TransportClosed {
            server: "linear".to_string(),
            client_id: 1,
        };
        let payload = build_payload("sess1", &key, &ev);
        assert_eq!(payload.name, "linear");
        assert_eq!(payload.status, McpServerStatus::Unavailable);
        assert_eq!(payload.reason, McpServerStatusReason::TransportClosed);
        assert_eq!(payload.detail, None);
        assert!(payload.tools.is_none());
    }

    /// Contract: HandshakeFailed `reason` is surfaced verbatim (full
    /// error, no sanitization/truncation) so debugging is unobstructed.
    #[test]
    fn payload_passes_full_handshake_reason() {
        let key = ("linear".to_string(), McpClientEventKind::HandshakeFailed);
        let ev = McpClientEvent::HandshakeFailed {
            server: "linear".to_string(),
            // Internal service names and full length must pass through
            // untouched — the UI shows the raw error.
            reason: "cli-chat-proxy returned 502".to_string(),
        };
        let payload = build_payload("sess1", &key, &ev);
        assert_eq!(payload.status, McpServerStatus::Unavailable);
        assert_eq!(payload.reason, McpServerStatusReason::HandshakeFailed);
        let detail = payload.detail.expect("detail set on handshake failure");
        assert_eq!(
            detail, "cli-chat-proxy returned 502",
            "reason must be passed through verbatim, got: {detail}",
        );
    }

    /// Handshake auth wording on a spawned local client stays `Unavailable`
    /// (local recovery is the OAuth path, not `server_status` NeedsAuth).
    #[test]
    fn local_handshake_auth_rejection_stays_unavailable() {
        let key = ("github".to_string(), McpClientEventKind::HandshakeFailed);
        let ev = McpClientEvent::HandshakeFailed {
            server: "github".to_string(),
            reason: "401 Unauthorized".to_string(),
        };
        let payload = build_payload("sess1", &key, &ev);
        assert_eq!(payload.source, McpServerSource::Local);
        assert_eq!(payload.status, McpServerStatus::Unavailable);
        assert_eq!(payload.reason, McpServerStatusReason::HandshakeFailed);
    }

    /// Gateway catalog ids are `Managed`; `echo_agent_com_*` spawned names are not.
    #[test]
    fn classify_source_gateway_is_managed() {
        assert_eq!(
            classify_source("managed_gateway:linear"),
            McpServerSource::Managed
        );
        assert_eq!(
            classify_source("echo_agent_com_linear"),
            McpServerSource::Local
        );
        assert_eq!(classify_source("github"), McpServerSource::Local);
    }

    /// Snapshot of the wire shape for one TransportClosed status push.
    ///
    /// Locks the camelCase field naming, lowercase enum values, and
    /// the `tools: null` placeholder. Update the expected JSON
    /// alongside any schema bumps so the wire contract and code stay in
    /// sync.
    #[test]
    fn server_status_payload_snapshot() {
        let key = ("github".to_string(), McpClientEventKind::TransportClosed);
        let ev = McpClientEvent::TransportClosed {
            server: "github".to_string(),
            client_id: 1,
        };
        let payload = build_payload("sess-1", &key, &ev);
        let json = serde_json::to_value(&payload).unwrap();
        let expected = serde_json::json!({
            "sessionId": "sess-1",
            "name": "github",
            "source": "local",
            "status": "unavailable",
            "reason": "transport_closed",
            "tools": serde_json::Value::Null,
        });
        assert_eq!(json, expected);
    }

    /// Regression guard: a first-time successful
    /// `ensure_initialized` fires `McpClientEvent::Ready` and the
    /// dispatcher must surface it as `reason=initialized` — NOT
    /// `restart_succeeded` (which is reserved for the auto-
    /// restart path).
    #[test]
    fn ready_event_maps_to_initialized_not_restart_succeeded() {
        let key = ("github".to_string(), McpClientEventKind::Ready);
        let ev = McpClientEvent::Ready {
            server: "github".to_string(),
        };
        let payload = build_payload("sess-1", &key, &ev);
        assert_eq!(payload.status, McpServerStatus::Ready);
        assert_eq!(payload.reason, McpServerStatusReason::Initialized);
        assert_ne!(payload.reason, McpServerStatusReason::RestartSucceeded);
        // Wire-level check: serializes to `"initialized"`.
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["reason"], "initialized");
    }

    #[test]
    fn managed_token_refreshed_still_deserializes() {
        let reason: McpServerStatusReason =
            serde_json::from_str("\"managed_token_refreshed\"").unwrap();
        assert_eq!(reason, McpServerStatusReason::ManagedTokenRefreshed);
    }

    /// A `TransportClosed` carrying the registered client's identity
    /// must remove it from `owned_clients`.
    #[tokio::test]
    async fn dispatcher_drops_dead_clients_on_transport_closed() {
        use echo_agent_mcp::servers::{McpClient, McpState};
        use std::sync::Arc as StdArc;

        let mcp_state = StdArc::new(TokioMutex::new(McpState::new(vec![])));
        // Pre-populate with a stub client so we have something to remove.
        let github = StdArc::new(McpClient::stub("github"));
        let github_id = github.client_id();
        {
            let mut guard = mcp_state.lock().await;
            guard.owned_clients.insert("github".to_string(), github);
            guard
                .owned_clients
                .insert("linear".to_string(), StdArc::new(McpClient::stub("linear")));
            assert_eq!(guard.owned_clients.len(), 2);
        }

        let mut win = CoalescedWindow::default();
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "github".to_string(),
                client_id: github_id,
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::ToolsChanged {
                server: "linear".to_string(),
            },
        );

        // No HTTP servers configured → "github" (stdio) is evicted.
        let dead = collect_close_candidates(&win, &HashSet::new());
        assert_eq!(
            dead,
            vec![DeadClient {
                server: "github".to_string(),
                closed: HashSet::from([github_id]),
            }]
        );
        let stale = drop_dead_clients(&mcp_state, &dead).await;
        assert!(stale.is_empty(), "a matching id is not a stale event");

        let guard = mcp_state.lock().await;
        assert!(!guard.owned_clients.contains_key("github"));
        assert!(
            guard.owned_clients.contains_key("linear"),
            "ToolsChanged must not remove the client; only TransportClosed does",
        );
    }

    /// Only `TransportClosed` participates in the drop path —
    /// `ConfigRemoved` is deliberately excluded (see
    /// `collect_close_candidates` doc).
    #[test]
    fn collect_close_candidates_picks_transport_closed_only() {
        let mut win = CoalescedWindow::default();
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "a".to_string(),
                client_id: 7,
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::ConfigDiff {
                added: vec![],
                removed: vec!["b".to_string()],
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::ToolsChanged {
                server: "c".to_string(),
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::Ready {
                server: "d".to_string(),
            },
        );

        let dead = collect_close_candidates(&win, &HashSet::new());
        assert_eq!(
            dead,
            vec![DeadClient {
                server: "a".to_string(),
                closed: HashSet::from([7]),
            }],
            "only TransportClosed collects; ConfigRemoved/ToolsChanged/Ready must not",
        );
    }

    /// Closed ids accumulate across the window: the current client's
    /// close must evict it even when a stale predecessor's close wins
    /// the buffer's last-write-wins slot.
    #[tokio::test]
    async fn window_accumulates_all_closed_ids_and_evicts_current_client() {
        use echo_agent_mcp::servers::{McpClient, McpState};
        use std::sync::Arc as StdArc;

        let old_client = StdArc::new(McpClient::stub("demo-mcp"));
        let current = StdArc::new(McpClient::stub("demo-mcp"));

        let mcp_state = StdArc::new(TokioMutex::new(McpState::new(vec![])));
        mcp_state
            .lock()
            .await
            .owned_clients
            .insert("demo-mcp".to_string(), StdArc::clone(&current));

        // The CURRENT client's close arrives first; the STALE one
        // arrives last and wins the buffer's last-write-wins slot.
        let mut win = CoalescedWindow::default();
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "demo-mcp".to_string(),
                client_id: current.client_id(),
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "demo-mcp".to_string(),
                client_id: old_client.client_id(),
            },
        );
        assert_eq!(
            win.buf.len(),
            1,
            "wire dedup unchanged: one buffered entry per (server, kind)",
        );

        let dead = collect_close_candidates(&win, &HashSet::new());
        let stale = drop_dead_clients(&mcp_state, &dead).await;

        assert!(
            !mcp_state
                .lock()
                .await
                .owned_clients
                .contains_key("demo-mcp"),
            "the current client's close must evict it even when the stale \
             event wins the buffer's last-write-wins slot",
        );
        assert!(
            stale.is_empty(),
            "an eviction window is not stale — status/restart must proceed",
        );
    }

    /// Remove+re-add race: a stale `TransportClosed` whose id belongs
    /// to an already-replaced client must NOT evict the replacement
    /// registered under the same name, and must be reported stale.
    #[tokio::test]
    async fn stale_transport_closed_does_not_evict_replacement_client() {
        use echo_agent_mcp::servers::{McpClient, McpState};
        use std::sync::Arc as StdArc;

        let old_client = StdArc::new(McpClient::stub("demo-mcp"));
        let old_id = old_client.client_id();
        let replacement = StdArc::new(McpClient::stub("demo-mcp"));
        assert_ne!(old_id, replacement.client_id(), "ids must be unique");

        let mcp_state = StdArc::new(TokioMutex::new(McpState::new(vec![])));
        // The config diff already removed `old_client` and the
        // background handshake inserted the replacement under the
        // same name — the state the dispatcher observes at flush time.
        mcp_state
            .lock()
            .await
            .owned_clients
            .insert("demo-mcp".to_string(), StdArc::clone(&replacement));

        let dead = vec![DeadClient {
            server: "demo-mcp".to_string(),
            closed: HashSet::from([old_id]),
        }];
        let stale = drop_dead_clients(&mcp_state, &dead).await;
        assert_eq!(
            stale,
            vec!["demo-mcp".to_string()],
            "a skipped eviction must be reported stale so the caller \
             suppresses the status push / restart for it",
        );

        let guard = mcp_state.lock().await;
        let current = guard
            .owned_clients
            .get("demo-mcp")
            .expect("replacement client must survive a stale TransportClosed");
        assert!(
            StdArc::ptr_eq(current, &replacement),
            "the surviving entry must be the replacement instance",
        );
    }

    /// HTTP recovery contract: an HTTP server's `TransportClosed` must
    /// NOT be evicted (it is recovered in place), while a stdio
    /// `TransportClosed` still evicts. `ConfigRemoved` never evicts here
    /// (the config diff already dropped the old client synchronously).
    #[test]
    fn collect_close_candidates_keeps_http_transport_closed() {
        let mut win = CoalescedWindow::default();
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "http-mcp-server".to_string(),
                client_id: 1,
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "local_stdio".to_string(),
                client_id: 2,
            },
        );
        insert_event(
            &mut win,
            McpClientEvent::ConfigDiff {
                added: vec![],
                removed: vec!["http-mcp-server".to_string()],
            },
        );

        let http: HashSet<McpServerName> = ["http-mcp-server".to_string()].into_iter().collect();

        // stdio TransportClosed drops; http-mcp-server's TransportClosed is kept
        // (HTTP → recovered in place); ConfigRemoved never evicts.
        let dead = collect_close_candidates(&win, &http);
        assert_eq!(
            dead,
            vec![DeadClient {
                server: "local_stdio".to_string(),
                closed: HashSet::from([2]),
            }],
        );

        // Recovery candidates: only http-mcp-server's TransportClosed.
        let recover = collect_http_transport_closed(&win.buf, &http);
        assert_eq!(recover, vec!["http-mcp-server".to_string()]);
    }

    fn http_cfg(name: &str) -> agent_client_protocol::McpServer {
        agent_client_protocol::McpServer::Http(
            agent_client_protocol::McpServerHttp::new(
                name.to_string(),
                format!("https://example.test/{name}"),
            )
            .headers(vec![]),
        )
    }

    fn stdio_cfg(name: &str) -> agent_client_protocol::McpServer {
        agent_client_protocol::McpServer::Stdio(
            agent_client_protocol::McpServerStdio::new(
                name.to_string(),
                std::path::PathBuf::from("x"),
            )
            .args(vec![])
            .env(vec![]),
        )
    }

    /// `recoverable_http_servers` keeps only non-disabled HTTP/SSE entries —
    /// the same predicate as the recovery gate.
    #[test]
    fn recoverable_http_servers_excludes_stdio_and_disabled() {
        let configs = vec![
            http_cfg("http-mcp-server"),
            http_cfg("echo_agent_com_slack"),
            http_cfg("admin_off"),    // disabled
            stdio_cfg("local_stdio"), // stdio
        ];
        let disabled: HashSet<String> = ["admin_off".to_string()].into_iter().collect();
        let got = recoverable_http_servers(&configs, &disabled);
        let want: HashSet<String> = [
            "http-mcp-server".to_string(),
            "echo_agent_com_slack".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
    }

    /// Scope guard: a disabled HTTP server still present
    /// in `configs` must be EVICTED, not orphaned. Since
    /// `recoverable_http_servers` excludes it, it lands in the drop set and
    /// not the recovery set — matching the recovery gate, which also
    /// rejects disabled names.
    #[test]
    fn disabled_http_server_is_evicted_not_recovered() {
        let configs = vec![http_cfg("admin_off")];
        let disabled: HashSet<String> = ["admin_off".to_string()].into_iter().collect();
        let http_servers = recoverable_http_servers(&configs, &disabled);
        assert!(
            http_servers.is_empty(),
            "disabled HTTP server is not recoverable"
        );

        let mut win = CoalescedWindow::default();
        insert_event(
            &mut win,
            McpClientEvent::TransportClosed {
                server: "admin_off".to_string(),
                client_id: 3,
            },
        );
        assert_eq!(
            collect_close_candidates(&win, &http_servers),
            vec![DeadClient {
                server: "admin_off".to_string(),
                closed: HashSet::from([3]),
            }],
            "disabled HTTP server must be evicted",
        );
        assert!(
            collect_http_transport_closed(&win.buf, &http_servers).is_empty(),
            "disabled HTTP server must not be a recovery candidate",
        );
    }

    // ── Integration test: end-to-end run_dispatcher
    //    with restart_actions wired. Pre-fix, `flush_window` marked
    //    `shutting_down` on every `TransportClosed`, which then
    //    short-circuited `maybe_schedule_restart` and caused
    //    auto-restart to never fire in production. This test drives a
    //    real `run_dispatcher` task end-to-end and — critically —
    //    wires the production `SharedShutdownState`
    //    into the test mock's `is_in_shutting_down` so the dispatcher
    //    ↔ actions binding is genuinely exercised. Pre-fix flush
    //    semantics would mark `"svr"` in the shared state, the mock
    //    would observe it, and the test would FAIL — closing the
    //    real regression loop.

    /// Construct an `AcpAgentGatewaySender` whose receiver half is
    /// dropped immediately. All `forward_fire_and_forget` calls
    /// silently no-op. Suitable only for tests that don't assert on
    /// wire payloads (renamed from `dummy_gateway`
    /// to make the discard semantics explicit at the call site).
    fn discard_gateway() -> echo_agent_acp::AcpAgentGatewaySender {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        echo_agent_acp::AcpAgentGatewaySender::new(tx)
    }

    /// `RestartActions` test double for the integration test.
    ///
    /// Uses `RefCell` for internal state, matching
    /// the `MockActions` convention in `mcp_restart.rs`. Both
    /// doubles model the same `?Send` trait on a single-threaded
    /// `LocalSet`; sharing the primitive removes a footgun for
    /// future maintainers.
    ///
    /// `is_in_shutting_down` consults a shared
    /// `SharedShutdownState`. The integration test below wires the
    /// SAME `SharedShutdownState` into both the dispatcher (via
    /// `run_dispatcher`'s `shutdown` parameter) and the mock, so the
    /// dispatcher's `flush_window` mutations are observed by the
    /// mock — mirroring how production `SessionRestartActions`
    /// reads from the same Arc.
    struct CountingActions {
        configured: std::cell::RefCell<HashSet<String>>,
        respawn_calls: std::cell::RefCell<Vec<String>>,
        shutdown: SharedShutdownState,
    }

    impl CountingActions {
        fn new(shutdown: SharedShutdownState) -> Self {
            Self {
                configured: std::cell::RefCell::new(HashSet::new()),
                respawn_calls: std::cell::RefCell::new(Vec::new()),
                shutdown,
            }
        }
        fn configure(&self, name: &str) {
            self.configured.borrow_mut().insert(name.to_string());
        }
        fn respawn_calls(&self) -> Vec<String> {
            self.respawn_calls.borrow().clone()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl crate::session::mcp_restart::RestartActions for CountingActions {
        async fn is_stdio_server_configured(&self, server: &str) -> bool {
            self.configured.borrow().contains(server)
        }
        fn is_in_shutting_down(&self, server: &str) -> bool {
            self.shutdown
                .lock()
                .expect("ShutdownState mutex poisoned")
                .is_shutting_down(server)
        }
        async fn respawn_stdio(&self, server: &str) -> Result<(), String> {
            self.respawn_calls.borrow_mut().push(server.to_string());
            Ok(())
        }
        fn push_status(&self, _payload: &crate::session::mcp_dispatcher::McpServerStatusPayload) {}
    }

    /// End-to-end: a `TransportClosed` event flowing through
    /// `run_dispatcher` schedules a `respawn_stdio` call.
    ///
    /// The mock's `is_in_shutting_down` reads
    /// from the same `SharedShutdownState` the dispatcher mutates.
    /// Pre-C1-fix, the dispatcher's `flush_window` would mark
    /// `"svr"` on `TransportClosed`, the mock would observe `true`,
    /// `maybe_schedule_restart` would short-circuit with
    /// `reason=shutting_down`, and `respawn_calls` would stay
    /// empty — making this test FAIL. With the fix, only
    /// `ConfigRemoved` marks the set, the mock observes `false` for
    /// `"svr"`, and the restart task fires.
    ///
    /// Uses `start_paused` + explicit
    /// `tokio::time::advance` instead of a 1500 ms wall-clock sleep.
    /// Deterministic under loaded CI.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn run_dispatcher_schedules_restart_on_transport_closed() {
        use echo_agent_mcp::servers::McpState;

        let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
        let shutdown = new_shutdown_state();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = discard_gateway();

        // Dispatcher AND mock share the SAME
        // `SharedShutdownState` — `flush_window` writes, the mock
        // reads.
        let actions = Rc::new(CountingActions::new(Arc::clone(&shutdown)));
        actions.configure("svr");
        let actions_for_assert = Rc::clone(&actions);
        let restart_actions: Rc<dyn crate::session::mcp_restart::RestartActions> = actions;

        // run_dispatcher is `!Send` (gateway + actions both LocalSet
        // bound). Run on a LocalSet.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let dispatcher = tokio::task::spawn_local(run_dispatcher(
                    "sess-1".to_string(),
                    rx,
                    gateway,
                    mcp_state,
                    shutdown,
                    Some(restart_actions),
                    std::path::PathBuf::from("."),
                ));

                // Send the event the C1 bug suppressed.
                tx.send(McpClientEvent::TransportClosed {
                    server: "svr".to_string(),
                    client_id: 1,
                })
                .unwrap();

                // Let dispatcher poll: collect_window receives the
                // first event and starts the 50 ms timeout_at.
                tokio::task::yield_now().await;
                // Advance past the 50 ms collect_window deadline so
                // the timeout fires, flush_window runs, and
                // maybe_schedule_restart spawns auto_restart_stdio.
                tokio::time::advance(Duration::from_millis(60)).await;
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                // Advance past BACKOFF[0] = 1 s so the spawned
                // auto_restart_stdio's first sleep elapses and
                // respawn_stdio is invoked.
                tokio::time::advance(Duration::from_secs(1)).await;
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }

                let calls = actions_for_assert.respawn_calls();
                assert_eq!(
                    calls,
                    vec!["svr".to_string()],
                    "TransportClosed must schedule exactly one respawn_stdio call \
                     (regression guard)",
                );

                dispatcher.abort();
            })
            .await;
    }

    /// End-to-end: a stale `TransportClosed` must be fully inert in
    /// `run_dispatcher` — replacement stays registered, no status
    /// push, no restart (even though the server is stdio-configured).
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn run_dispatcher_stale_transport_closed_is_fully_inert() {
        use echo_agent_mcp::servers::{McpClient, McpState};
        use std::sync::Arc as StdArc;

        let old_client = StdArc::new(McpClient::stub("svr"));
        let replacement = StdArc::new(McpClient::stub("svr"));
        let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
        mcp_state
            .lock()
            .await
            .owned_clients
            .insert("svr".to_string(), StdArc::clone(&replacement));

        let shutdown = new_shutdown_state();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Capturing gateway: keep the receiver so the test can assert
        // that NOTHING was pushed for the stale event.
        let (gw_tx, mut gw_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = echo_agent_acp::AcpAgentGatewaySender::new(gw_tx);

        let actions = Rc::new(CountingActions::new(Arc::clone(&shutdown)));
        // Configured-as-stdio on purpose: proves the no-restart
        // outcome comes from the stale-event suppression, not from
        // the stdio guard rail.
        actions.configure("svr");
        let actions_for_assert = Rc::clone(&actions);
        let restart_actions: Rc<dyn crate::session::mcp_restart::RestartActions> = actions;

        let state_for_dispatcher = Arc::clone(&mcp_state);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let dispatcher = tokio::task::spawn_local(run_dispatcher(
                    "sess-1".to_string(),
                    rx,
                    gateway,
                    state_for_dispatcher,
                    Arc::clone(&shutdown),
                    Some(restart_actions),
                    std::path::PathBuf::from("."),
                ));

                tx.send(McpClientEvent::TransportClosed {
                    server: "svr".to_string(),
                    client_id: old_client.client_id(),
                })
                .unwrap();

                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_millis(60)).await;
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                // Past BACKOFF[0]: a (wrongly) scheduled restart would
                // have respawned by now.
                tokio::time::advance(Duration::from_secs(1)).await;
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }

                let guard = mcp_state.lock().await;
                let current = guard
                    .owned_clients
                    .get("svr")
                    .expect("replacement must survive the stale event");
                assert!(
                    StdArc::ptr_eq(current, &replacement),
                    "registered client must still be the replacement",
                );
                drop(guard);
                assert!(
                    actions_for_assert.respawn_calls().is_empty(),
                    "a stale TransportClosed must not schedule a restart",
                );
                assert!(
                    gw_rx.try_recv().is_err(),
                    "a stale TransportClosed must not push any server_status",
                );

                dispatcher.abort();
            })
            .await;
    }

    /// Direct mark-semantics: `flush_window` only marks
    /// `shutting_down` on `ConfigRemoved`, never on `TransportClosed`
    /// Stand-alone regression guard
    /// complementing the end-to-end test above.
    #[tokio::test]
    async fn flush_window_marks_shutting_down_only_on_config_removed() {
        let shutdown = new_shutdown_state();
        let gateway = discard_gateway();

        // TransportClosed alone: must NOT mark.
        let mut buf: HashMap<(McpServerName, McpClientEventKind), McpClientEvent> = HashMap::new();
        buf.insert(
            ("crashed".to_string(), McpClientEventKind::TransportClosed),
            McpClientEvent::TransportClosed {
                server: "crashed".to_string(),
                client_id: 1,
            },
        );
        flush_window("s", buf, &shutdown, &gateway);
        assert!(
            !shutdown.lock().unwrap().is_shutting_down("crashed"),
            "TransportClosed alone must not mark shutting_down (C1 fix)",
        );

        // ConfigRemoved: must mark.
        let mut buf: HashMap<(McpServerName, McpClientEventKind), McpClientEvent> = HashMap::new();
        buf.insert(
            ("removed".to_string(), McpClientEventKind::ConfigRemoved),
            McpClientEvent::ConfigRemoved {
                server: "removed".to_string(),
            },
        );
        flush_window("s", buf, &shutdown, &gateway);
        assert!(
            shutdown.lock().unwrap().is_shutting_down("removed"),
            "ConfigRemoved must mark shutting_down (kill_on_drop guard rail)",
        );

        // Ready clears.
        let mut buf: HashMap<(McpServerName, McpClientEventKind), McpClientEvent> = HashMap::new();
        buf.insert(
            ("removed".to_string(), McpClientEventKind::Ready),
            McpClientEvent::Ready {
                server: "removed".to_string(),
            },
        );
        flush_window("s", buf, &shutdown, &gateway);
        assert!(
            !shutdown.lock().unwrap().is_shutting_down("removed"),
            "Ready must clear shutting_down",
        );
    }
}
