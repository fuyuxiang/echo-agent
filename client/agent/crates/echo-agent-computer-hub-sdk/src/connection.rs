//! Per-`(url, principal)` WebSocket connection actor.
//!
//! # Why this exists
//!
//! Multiple [`crate::ToolServer`] instances in the same process MAY
//! attach to the same server URL with the same credential. Opening one
//! socket per server would multiply server-side connection cost, fan-out
//! the per-tool ack chatter, and make per-frame envelope checks
//! ambiguous (the server can't tell which of N sockets owns a session
//! binding). The pool collapses every `(url, principal)` to one
//! [`HubConnection`]; refcounted session bindings make the collapse
//! safe.
//!
//! # The reconnect / replay state machine
//!
//! When the underlying socket drops, in-flight `tool_call_request`
//! responses CANNOT be recovered (the server holds no replay log). The
//! connection actor therefore:
//!
//! 1. Drains every parked response waiter with
//!    [`crate::ClientError::NetworkError`] so callers can fast-fail
//!    instead of deadlocking.
//! 2. Reconnects with exponential backoff, full jitter, capped at the last slot.
//! 3. Re-runs the `hello` handshake.
//! 4. The ToolServer replays `serve{session_id, tools}` per active
//!    session via the on_reconnect callback. The server auto-registers
//!    sessions from `serve` so no separate wire call is needed.
//! 5. Drains any outbound frames that buffered during step 1-4.
use crate::auth::{AuthCredential, AuthProvider, PrincipalKey};
use crate::demux::Demux;
use crate::error::ClientError;
use crate::handshake::send_hello;
use crate::refcount::RefCountedSet;
use echo_agent_tool_protocol::{
    ConnectionId, ConnectionKind, JsonRpcId, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion,
    Method, PingFrame, PongFrame, ResponseOutcome, SessionId,
};
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use futures::{SinkExt, Stream, StreamExt};
use http::HeaderName;
use http::header::HeaderValue;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use url::Url;
/// Outbound mpsc bound. Picked to match the server's per-actor outbound
/// buffer so a single-process roundtrip never dead-blocks on sender
/// capacity.
const OUTBOUND_BUFFER: usize = 256;
/// Writer control channel depth. Must fit a liveness `Close` + `Pause`
/// while still leaving room for `Resume` if the writer is mid-`sink.send`.
const WRITER_CTL_CAPACITY: usize = 4;
/// App-pong / priority outbound depth. Small and independent of the data
/// outbound buffer so heartbeats are not shed by tool-call backpressure.
const PRIORITY_OUTBOUND_CAPACITY: usize = 16;
/// Bound for the best-effort WS Close on a liveness kill. A silently dead
/// peer with a full TCP send buffer must not block Pause/Resume forever.
const WRITER_CLOSE_SEND_TIMEOUT: Duration = Duration::from_secs(2);
/// Backoff schedule (in ms) for reconnect attempts. The last value is
/// reused for any further attempts and is the documented cap (`10s`).
/// Each wait is `Uniform(0, min(cap, max(slot, SPREAD_FLOOR)))`.
const RECONNECT_BACKOFF_MS: &[u64] = &[100, 200, 500, 1_000, 2_000, 5_000, 10_000];
/// Lower bound on the full-jitter window. ±25 % of the 100 ms first slot
/// is only a 50 ms spread — phase-locked reconnects from a large client
/// fleet can overwhelm a recovering server once handshake+replay exceeds
/// that window (peak concurrent handshakes approaches N). Uniform over
/// ≥1 s spreads the same herd across roughly one slot. Capped by the
/// schedule's last slot so a short test/override table stays fast. Keep
/// the floor well below typical client disconnect-grace timers so a
/// delayed reconnect is not mistaken for a permanent drop.
const RECONNECT_SPREAD_FLOOR: Duration = Duration::from_secs(1);
/// Prior connection must have lived this long before a new outage resets
/// `attempt`. Shorter flaps keep climbing so a crash-loop server or a
/// drain followed immediately by another drop cannot pin the fleet on
/// slot 1.
const RECONNECT_ATTEMPT_RESET_AFTER: Duration = Duration::from_secs(10);
/// Per-process counter mixed into each connection's jitter seed so two
/// clients constructed in the same instant still de-phase on attempt 1.
static NEXT_RECONNECT_JITTER_SEED: AtomicU64 = AtomicU64::new(1);
/// Floor for the per-attempt reconnect budget: a small liveness override
/// must not shrink it below what a WAN handshake + session replay needs,
/// or the retry loop would livelock aborting every attempt at the bound.
const RECONNECT_ATTEMPT_MIN_BUDGET: Duration = Duration::from_secs(30);
/// Per-attempt reconnect budget: the liveness deadline, floored so liveness
/// tuning bounds detection, not connection establishment.
fn reconnect_attempt_budget(liveness_deadline: Duration) -> Duration {
    liveness_deadline.max(RECONNECT_ATTEMPT_MIN_BUDGET)
}
/// Per-attempt budget for the initial connect (WebSocket upgrade +
/// hello/hello_ack). Neither `connect_async` nor the hello_ack wait is
/// otherwise bounded, so a peer that accepts the socket but never answers
/// (e.g. a hub instance draining mid-roll) would hang the caller
/// indefinitely, burning the embedder's own readiness budget on one dead
/// attempt.
const INITIAL_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Initial-connect attempts before the error surfaces to the caller. Waits
/// between attempts come from the reconnect backoff schedule (jittered), so
/// a fleet cold-starting into a degraded hub de-phases its retries.
const INITIAL_CONNECT_MAX_ATTEMPTS: u32 = 3;
/// Default WebSocket keepalive ping cadence when a connection does not
/// override [`ConnectionTuning::ws_ping_interval`].
const DEFAULT_WS_PING_INTERVAL: Duration = Duration::from_secs(30);
const SERVE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
const SERVE_MAX_ATTEMPTS: u32 = 3;
const CLOCK_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const CLOCK_JUMP_ACCUM_MIN_MS: u64 = 100;
const CLOCK_JUMP_REPORT_MIN_MS: u64 = 2_000;
type WriteErrorSlot = Arc<parking_lot::Mutex<Option<String>>>;
struct HealthState {
    last_inbound: Instant,
    mono_ref: Instant,
    wall_ref: SystemTime,
    clock_jump_accum_ms: u64,
}
struct HealthSnapshot {
    last_inbound: Instant,
    /// Monotonic time elapsed since the last probe window rolled (the most
    /// recent RTT proof — WS/app pong — or 5s clock probe) — NOT since
    /// connection start. Healthy traffic keeps this small (<= ~5s); the
    /// meaningful freeze signal in this snapshot is `clock_jump_ms`.
    since_last_probe_monotonic_ms: u64,
    /// Wall-clock time elapsed over the same probe window as
    /// `since_last_probe_monotonic_ms`.
    since_last_probe_wall_ms: u64,
    clock_jump_ms: u64,
}
struct ConnHealth {
    state: parking_lot::Mutex<HealthState>,
}
impl ConnHealth {
    fn new() -> Self {
        Self {
            state: parking_lot::Mutex::new(Self::fresh_state()),
        }
    }
    fn fresh_state() -> HealthState {
        HealthState {
            last_inbound: Instant::now(),
            mono_ref: Instant::now(),
            wall_ref: SystemTime::now(),
            clock_jump_accum_ms: 0,
        }
    }
    fn deltas(state: &HealthState) -> (u64, u64) {
        let mono_ms = state.mono_ref.elapsed().as_millis() as u64;
        let wall_ms = SystemTime::now()
            .duration_since(state.wall_ref)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        (mono_ms, wall_ms)
    }
    fn roll(state: &mut HealthState) {
        let (mono_ms, wall_ms) = Self::deltas(state);
        let excess = wall_ms.saturating_sub(mono_ms);
        if excess >= CLOCK_JUMP_ACCUM_MIN_MS {
            state.clock_jump_accum_ms = state.clock_jump_accum_ms.saturating_add(excess);
        }
        state.mono_ref = Instant::now();
        state.wall_ref = SystemTime::now();
    }
    /// Record RTT proof (WS/app pong). Hub→client pings/data must not call
    /// this — they are one-way and would zero `detect_ms` / `silent_gap_ms`
    /// during a mute that still expires the liveness deadline.
    fn record_inbound(&self) {
        let mut state = self.state.lock();
        Self::roll(&mut state);
        state.last_inbound = Instant::now();
    }
    fn refresh_clock(&self) {
        let mut state = self.state.lock();
        Self::roll(&mut state);
    }
    fn snapshot(&self) -> HealthSnapshot {
        let state = self.state.lock();
        let (mono_ms, wall_ms) = Self::deltas(&state);
        let excess = wall_ms.saturating_sub(mono_ms);
        let total =
            state
                .clock_jump_accum_ms
                .saturating_add(if excess >= CLOCK_JUMP_ACCUM_MIN_MS {
                    excess
                } else {
                    0
                });
        HealthSnapshot {
            last_inbound: state.last_inbound,
            since_last_probe_monotonic_ms: mono_ms,
            since_last_probe_wall_ms: wall_ms,
            clock_jump_ms: if total >= CLOCK_JUMP_REPORT_MIN_MS {
                total
            } else {
                0
            },
        }
    }
    fn reset(&self) {
        *self.state.lock() = Self::fresh_state();
    }
}
enum DisconnectCause {
    CloseFrame(Option<u16>),
    Eof,
    ReadError(String),
    WriteError(String),
    Forced,
    /// No RTT proof (WS/app pong or non-ping data) within the inbound-liveness
    /// deadline — return path is silently dead.
    LivenessDeadline,
}
impl DisconnectCause {
    fn label(&self) -> &'static str {
        match self {
            Self::CloseFrame(_) => "close_frame",
            Self::Eof => "eof",
            Self::ReadError(_) => "transport_read_error",
            Self::WriteError(_) => "transport_write_error",
            Self::Forced => "forced",
            Self::LivenessDeadline => "liveness_deadline",
        }
    }
    fn close_code(&self) -> Option<u16> {
        match self {
            Self::CloseFrame(code) => *code,
            _ => None,
        }
    }
    fn detail(&self) -> Option<&str> {
        match self {
            Self::ReadError(detail) | Self::WriteError(detail) => Some(detail),
            _ => None,
        }
    }
    /// Bounded classification of transport error detail for metrics. Collapses
    /// free-form OS/tungstenite messages into a small allowlist so reconnect
    /// storms can be attributed without high-cardinality labels.
    fn detail_class(&self) -> Option<&'static str> {
        let detail = self.detail()?;
        Some(classify_transport_detail(detail))
    }
}
/// Map a transport error detail string to a bounded class label.
fn classify_transport_detail(detail: &str) -> &'static str {
    let d = detail.to_ascii_lowercase();
    if d.contains("connection reset") || d.contains("econnreset") || d.contains("reset by peer") {
        "connection_reset"
    } else if d.contains("broken pipe") || d.contains("epipe") {
        "broken_pipe"
    } else if d.contains("unexpected eof")
        || d.contains("connection closed")
        || d.contains("connection aborted without closing")
    {
        "unexpected_eof"
    } else if d.contains("timed out") || d.contains("timeout") || d.contains("etimedout") {
        "timeout"
    } else if d.contains("connection aborted") || d.contains("econnaborted") {
        "connection_aborted"
    } else {
        "other"
    }
}
struct OutageInfo {
    cause: DisconnectCause,
    prev_connection_id: Option<ConnectionId>,
    prev_connection_duration_ms: u64,
    last_inbound: Instant,
    detect_ms: u64,
    since_last_probe_monotonic_ms: u64,
    since_last_probe_wall_ms: u64,
    clock_jump_ms: u64,
}
enum DeadlineCallError {
    TimedOut(Duration),
    Other(ClientError),
}
impl From<DeadlineCallError> for ClientError {
    fn from(err: DeadlineCallError) -> Self {
        match err {
            DeadlineCallError::TimedOut(timeout) => {
                ClientError::NetworkError(format!("request timed out after {timeout:?}"))
            }
            DeadlineCallError::Other(e) => e,
        }
    }
}
struct WaiterGuard<'a> {
    demux: &'a Demux,
    request_id: &'a echo_agent_tool_protocol::RequestId,
}
impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let _ = self.demux.take_response_waiter(self.request_id);
    }
}
/// Process-wide default reconnect schedule, materialised once from
/// [`RECONNECT_BACKOFF_MS`]. Connections that do not override
/// [`ConnectionTuning::reconnect_backoff`] share this `Arc` (cheap clone,
/// no per-connect allocation).
fn default_reconnect_backoff() -> Arc<[Duration]> {
    static DEFAULT: std::sync::OnceLock<Arc<[Duration]>> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            RECONNECT_BACKOFF_MS
                .iter()
                .map(|&ms| Duration::from_millis(ms))
                .collect()
        })
        .clone()
}
/// Resolve a configured backoff schedule, falling back to the built-in
/// table when unset (or empty, which would be degenerate).
fn resolve_reconnect_backoff(configured: Option<Arc<[Duration]>>) -> Arc<[Duration]> {
    match configured {
        Some(schedule) if !schedule.is_empty() => schedule,
        _ => default_reconnect_backoff(),
    }
}
/// `None` → the 10 s production dwell. `Some`, including zero, is verbatim.
pub(crate) fn resolve_attempt_reset_after(configured: Option<Duration>) -> Duration {
    configured.unwrap_or(RECONNECT_ATTEMPT_RESET_AFTER)
}
/// Resolve the keepalive ping cadence, clamping an unset *or zero* value to
/// [`DEFAULT_WS_PING_INTERVAL`]. `tokio::time::interval` panics on a zero
/// period, so a configured `Duration::ZERO` (e.g. via
/// `with_ws_ping_interval(0)` or a `StatusConfig.ws_ping` of 0) must never
/// reach the writer task.
fn resolve_ws_ping_interval(configured: Option<Duration>) -> Duration {
    match configured {
        Some(interval) if !interval.is_zero() => interval,
        _ => DEFAULT_WS_PING_INTERVAL,
    }
}
/// Resolve the per-attempt initial-connect budget, clamping an unset *or
/// zero* value to [`INITIAL_CONNECT_ATTEMPT_TIMEOUT`] — a zero budget would
/// abort every attempt before the upgrade could complete.
fn resolve_initial_connect_attempt_timeout(configured: Option<Duration>) -> Duration {
    match configured {
        Some(timeout) if !timeout.is_zero() => timeout,
        _ => INITIAL_CONNECT_ATTEMPT_TIMEOUT,
    }
}
/// Whether an initial-connect failure is worth another attempt. Transport
/// failures (including the per-attempt timeout, which surfaces as
/// `NetworkError`) and server closes are transient; auth, config, protocol,
/// and insecure-scheme failures are deterministic and must surface
/// immediately.
fn initial_connect_retryable(err: &ClientError) -> bool {
    matches!(err, ClientError::NetworkError(_) | ClientError::Closed(_))
}
/// Resolve the inbound-liveness deadline, clamping an unset *or zero* value
/// to `min(4× ping, 120s)` — 120s at the default 30s ping, still under the
/// hub's ~150s idle timeout.
///
/// After RTT-only re-arm, hub app/WS pings no longer keep the timer alive.
/// 4× (capped) tolerates a few lost/coalesced pongs plus scheduling jitter
/// without racing hub 4408. Explicit overrides are honored verbatim; keep
/// them comfortably above the ping interval or a healthy-but-idle
/// connection will be churned.
fn resolve_ws_liveness_deadline(configured: Option<Duration>, ping_interval: Duration) -> Duration {
    match configured {
        Some(deadline) if !deadline.is_zero() => deadline,
        _ => ping_interval
            .saturating_mul(4)
            .min(Duration::from_secs(120)),
    }
}
/// Optional, default-preserving connection-tuning knobs carried from the
/// pool/builder into [`ConnectionConfig`]. `Default` leaves every value
/// `None`, reproducing the historical hardcoded behaviour — and lets
/// config constructors write `tuning: ConnectionTuning::default()` so new
/// knobs never churn every [`ConnectionConfig`] literal.
#[derive(Clone, Default)]
pub struct ConnectionTuning {
    /// Override for the keepalive ping cadence. `None` (or zero) ⇒
    /// [`DEFAULT_WS_PING_INTERVAL`].
    pub ws_ping_interval: Option<Duration>,
    /// Override for the inbound-liveness deadline: with no *round-trip*
    /// proof (WS/app pong) for this long, the reader declares the socket
    /// dead and reconnects. Hub app pings and one-way hub→client data do
    /// not re-arm. `None` (or zero) ⇒ `min(4× ping, 120s)`
    /// (see [`resolve_ws_liveness_deadline`]).
    pub ws_liveness_deadline: Option<Duration>,
    /// Override for the reconnect backoff schedule. `None` (or empty) ⇒
    /// the built-in [`RECONNECT_BACKOFF_MS`] table. Each wait is
    /// `Uniform(0, min(last_slot, max(slot, 1s)))` — not the literal slot.
    /// Stored as `Arc<[Duration]>` so it is shared per reconnect.
    pub reconnect_backoff: Option<Arc<[Duration]>>,
    /// How long the previous connection must have stayed up before a new
    /// outage resets the reconnect attempt counter. `None` ⇒ 10 s (one
    /// default cap period). `Some`, including zero, is honored verbatim
    /// (`Some(ZERO)` resets on every outage; tests use this).
    pub reconnect_attempt_reset_after: Option<Duration>,
    /// Allowlist of 4100–4199 close codes that fire
    /// [`ConnectionConfig::on_terminal_close`] then re-enter the reconnect
    /// loop instead of permanently stopping the actor. Empty (default)
    /// keeps the protocol contract: every terminal close is a one-way door.
    /// Only codes for a still-restorable session (e.g.
    /// [`CLOSE_CODE_SANDBOX_TERMINATED`]) belong here; one-way codes
    /// (force eviction, session expiry, admin disconnect, supersession)
    /// must not.
    pub reconnect_after_terminal_close_codes: Vec<u16>,
    /// Per-attempt budget for the initial connect (WebSocket upgrade +
    /// hello/hello_ack). `None` (or zero) ⇒
    /// [`INITIAL_CONNECT_ATTEMPT_TIMEOUT`].
    pub initial_connect_attempt_timeout: Option<Duration>,
}
/// Pool dedup key. Two connections are pooled together iff their
/// `(url, principal)` match.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ConnKey {
    /// Normalised URL (parsed by [`Url::parse`]).
    pub url: String,
    /// Principal projection of the [`AuthCredential`].
    pub principal: PrincipalKey,
}
impl std::fmt::Debug for ConnKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnKey")
            .field("url", &self.url)
            .field("principal", &self.principal)
            .finish()
    }
}
/// Reconnect-callback payload. Dispatched once per successful reconnect
/// so consumers can record metrics or surface UI hints.
#[derive(Debug, Clone)]
pub struct ReconnectEvent {
    /// Server-issued connection id of the FRESH connection (different
    /// from the dropped one).
    pub connection_id: ConnectionId,
    /// Number of session bindings replayed.
    pub sessions_replayed: usize,
    /// Reconnect attempt index since the last *stable* connection (1-based).
    /// Rapid flaps do not reset this, so a crash-loop climbs the ladder.
    /// After the prior socket stayed up for
    /// [`ConnectionTuning::reconnect_attempt_reset_after`] (default 10 s)
    /// the next outage starts at 1 again.
    pub attempt: u32,
}
/// Boxed reconnect callback.
pub type ReconnectCallback = Box<dyn Fn(ReconnectEvent) + Send + Sync + 'static>;
/// Boxed disconnect callback, fired when the live socket drops (before a
/// reconnect attempt) and on a terminal close.
pub type DisconnectCallback = Box<dyn Fn() + Send + Sync + 'static>;
/// Boxed terminal-close callback, fired with the WebSocket close code when
/// the server ends the connection in the 4100–4199 range. Default policy is
/// no reconnect; [`ConnectionTuning::reconnect_after_terminal_close_codes`]
/// opts the embedder into recovery after this callback. Always followed by
/// [`DisconnectCallback`] so readiness still flips.
pub type TerminalCloseCallback = Box<dyn Fn(u16) + Send + Sync + 'static>;
/// Boxed connect callback, fired once on the initial successful connect
/// after the writer keepalive loop has entered (so `/ready` cannot race
/// the first ping) and before the reader actor task spawns. It therefore
/// strictly happens-before any disconnect/reconnect callback, so a
/// connect/disconnect pair can never be observed out of order (e.g. a
/// readiness marker resurrected after the socket has already dropped).
pub type ConnectCallback = Box<dyn Fn() + Send + Sync + 'static>;
/// A live (or reconnecting) connection to the server.
///
/// Cheap to clone via the `Arc` returned from
/// [`crate::HubConnectionPool::get_or_connect`]. Methods on the inner
/// `HubConnection` are `&self` so multiple consumers can share the
/// same instance without external locking.
///
/// Dropping the last `Arc<HubConnection>` runs [`Drop`], which sends
/// a stop signal to the connection actor; the actor drains every
/// in-flight response waiter with [`ClientError::NetworkError`] and
/// exits asynchronously. [`Self::request_shutdown`] triggers the
/// same stop-and-drain sequence without giving up the `Arc`.
pub struct HubConnection {
    inner: Arc<HubConnectionInner>,
}
impl std::fmt::Debug for HubConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubConnection")
            .field("key", &self.inner.key)
            .field("kind", &self.inner.kind)
            .finish_non_exhaustive()
    }
}
/// Configuration for a [`HubConnection`].
///
/// Consumed by [`HubConnection::connect`]; not `Clone` because the
/// only path that wants a copy is the pool, and the pool builds a
/// fresh config per attempt rather than cloning.
pub struct ConnectionConfig {
    /// `ws://` or `wss://` URL of the server.
    pub url: Url,
    pub credential: Arc<dyn AuthProvider>,
    /// Connection role announced in the hello frame.
    pub kind: ConnectionKind,
    /// Optional reconnect-event callback.
    pub on_reconnect: Option<Arc<ReconnectCallback>>,
    /// Optional disconnect callback, fired when the live socket drops or the
    /// server sends a terminal close.
    pub on_disconnect: Option<Arc<DisconnectCallback>>,
    /// Optional terminal-close callback, fired with the close code on a
    /// 4100–4199 close, before [`Self::on_disconnect`]. The actor still
    /// stops afterwards unless the code is in
    /// [`ConnectionTuning::reconnect_after_terminal_close_codes`].
    pub on_terminal_close: Option<Arc<TerminalCloseCallback>>,
    /// Optional connect callback, fired once on the initial successful connect
    /// after the writer task enters its loop (happens-before reader start).
    /// The first keepalive may still be in flight or one scheduler quanta away.
    pub on_connect: Option<Arc<ConnectCallback>>,
    /// Stable server identity sent in the hello frame. Only meaningful
    /// for [`ConnectionKind::ToolServer`] connections.
    pub server_id: Option<echo_agent_tool_protocol::ServerId>,
    /// One-line server description for `servers.list`.
    pub server_description: Option<String>,
    /// Opaque metadata surfaced in `ServerInfo.metadata`.
    pub server_metadata: Option<serde_json::Value>,
    /// Optional override for the outbound mpsc bound. `None` uses the
    /// crate default (matched to the server's per-actor outbound
    /// buffer). Tests use this to exercise the
    /// bounded-wait fast-fail path without flooding production-sized
    /// buffers.
    pub outbound_buffer: Option<usize>,
    /// Optional tuning knobs (ping cadence, liveness deadline, reconnect
    /// backoff). `ConnectionTuning::default()` keeps every historical
    /// default.
    pub tuning: ConnectionTuning,
    /// When set, attached as an extra access header on every
    /// (re)connect, unconditionally. Harmless when the peer ignores it.
    pub alpha_test_key: Option<String>,
    /// Allow a plaintext `ws://` connection to a non-loopback host.
    /// Only enable when the transport is otherwise secured (e.g. a
    /// private network or TLS-terminating proxy) — otherwise the bearer
    /// credential crosses the network in cleartext.
    pub allow_insecure_ws: bool,
    /// Optional weak handle to the owning pool, set by
    /// [`crate::HubConnectionPool::get_or_connect`]. On a fatal
    /// handshake-auth failure the reconnect driver evicts its own pool
    /// entry through this so the next caller opens a fresh socket.
    /// `None` for the unpooled [`HubConnection::connect`] path (tests /
    /// one-shot) — nothing to evict. Weak so the pool↔connection edge
    /// is not an ownership cycle.
    pub on_fatal: Option<Weak<crate::pool::HubConnectionPool>>,
}
impl std::fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("url", &self.url.as_str())
            .field("credential", &self.credential)
            .field("kind", &self.kind)
            .field("allow_insecure_ws", &self.allow_insecure_ws)
            .field("on_reconnect", &self.on_reconnect.is_some())
            .finish()
    }
}
struct HubConnectionInner {
    key: ConnKey,
    kind: ConnectionKind,
    credential: Arc<dyn AuthProvider>,
    on_reconnect: Option<Arc<ReconnectCallback>>,
    on_disconnect: Option<Arc<DisconnectCallback>>,
    on_terminal_close: Option<Arc<TerminalCloseCallback>>,
    server_id: Option<echo_agent_tool_protocol::ServerId>,
    server_description: Option<String>,
    server_metadata: Option<serde_json::Value>,
    /// Attached as an extra access header on every (re)connect when set.
    alpha_test_key: Option<String>,
    /// Permit plaintext `ws://` to a non-loopback host (transport otherwise secured).
    allow_insecure_ws: bool,
    /// See [`ConnectionConfig::on_fatal`].
    on_fatal: Option<Weak<crate::pool::HubConnectionPool>>,
    /// Resolved reconnect backoff schedule (configured override or the
    /// built-in table). Resolved once at connect; shared per reconnect.
    reconnect_backoff: Arc<[Duration]>,
    /// Per-connection seed for reconnect backoff jitter. Distinct across
    /// clients in the same process so a simultaneous disconnect does not
    /// lock-step the first (or any) attempt.
    reconnect_jitter_seed: u64,
    /// Resolved stability dwell before `attempt` resets on a new outage.
    attempt_reset_after: Duration,
    /// Embedder opt-in: sorted allowlist of 4100–4199 close codes to
    /// reconnect after instead of exiting. Empty ⇒ never reconnect.
    reconnect_after_terminal_close_codes: Vec<u16>,
    /// Incremented at the start of each reconnect episode so jitter
    /// re-phases across outages of the same connection.
    outage_seq: AtomicU32,
    /// Outbound frames waiting to be written. Filled by `send_*`
    /// helpers; drained by the writer half of the actor.
    outbound_tx: mpsc::Sender<String>,
    /// Inbound demux state (response waiters + session inboxes).
    demux: Arc<Demux>,
    /// Refcounted bound-session set. Used by the reconnect path to
    /// re-issue `register_session` for every still-live session.
    bound_sessions: Arc<RefCountedSet<SessionId>>,
    /// Cached server-issued `connection_id`. Updated on every (re)connect.
    connection_id: Arc<Mutex<Option<ConnectionId>>>,
    /// Optional capabilities the server advertised in the most recent
    /// `hello_ack` (wire method strings). Refreshed on every (re)connect
    /// handshake. Empty when the ack carried none — on the wire that is
    /// indistinguishable from a server predating the field, so
    /// [`HubConnection::supports`] reports unknown in that case.
    hello_capabilities: parking_lot::RwLock<Vec<String>>,
    /// Monotonically-increasing JSON-RPC request id counter.
    next_request_id: std::sync::atomic::AtomicU64,
    /// Cancelled by the actor task once it has fully exited so
    /// `await_shutdown` resolves promptly. `CancellationToken` has
    /// persistent semantics so a wait that arrives AFTER the actor
    /// has already cancelled the token still wakes immediately.
    shutdown: CancellationToken,
    /// Stops the actor on `Drop`.
    stop_tx: mpsc::Sender<()>,
    reconnect_tx: mpsc::Sender<()>,
    early_notif_rx: parking_lot::Mutex<Option<broadcast::Receiver<Value>>>,
    health: ConnHealth,
    writer_error: WriteErrorSlot,
}
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
impl HubConnection {
    /// Open a brand-new [`HubConnection`] and spawn its actor task.
    ///
    /// The pool is the canonical caller; outside callers MAY use this
    /// for tests or one-shot programs but lose pool dedup.
    pub async fn connect(config: ConnectionConfig) -> Result<Arc<Self>, ClientError> {
        let key = ConnKey {
            url: config.url.as_str().to_owned(),
            principal: config.credential.principal_key(),
        };
        let ws_ping_interval = resolve_ws_ping_interval(config.tuning.ws_ping_interval);
        let ws_liveness_deadline =
            resolve_ws_liveness_deadline(config.tuning.ws_liveness_deadline, ws_ping_interval);
        if ws_liveness_deadline <= ws_ping_interval {
            warn!(
                ?ws_liveness_deadline,
                ?ws_ping_interval,
                "ws liveness deadline is not greater than the keepalive ping interval; healthy idle connections will be killed and reconnected every window"
            );
        }
        let reconnect_backoff = resolve_reconnect_backoff(config.tuning.reconnect_backoff);
        let attempt_reset_after =
            resolve_attempt_reset_after(config.tuning.reconnect_attempt_reset_after);
        let buffer = config.outbound_buffer.unwrap_or(OUTBOUND_BUFFER);
        let (outbound_tx, outbound_rx) = mpsc::channel::<String>(buffer);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
        let (reconnect_tx, reconnect_rx) = mpsc::channel::<()>(1);
        let demux = Arc::new(Demux::with_outbound(outbound_tx.clone()));
        let bound_sessions = Arc::new(RefCountedSet::<SessionId>::new());
        let connection_id = Arc::new(Mutex::new(None));
        let shutdown = CancellationToken::new();
        let budget =
            resolve_initial_connect_attempt_timeout(config.tuning.initial_connect_attempt_timeout);
        let initial_jitter_seed = new_reconnect_jitter_seed();
        let mut attempt: u32 = 0;
        let (sink, stream, ack) = loop {
            attempt += 1;
            let cred = config.credential.current();
            let attempt_result = match tokio::time::timeout(budget, async {
                let ws = open_socket(
                    &config.url,
                    &cred,
                    config.kind,
                    config.alpha_test_key.as_deref(),
                    config.allow_insecure_ws,
                )
                .await?;
                let (sink, stream) = ws.split();
                run_handshake(
                    sink,
                    stream,
                    config.kind,
                    config.server_id.clone(),
                    config.server_description.clone(),
                    config.server_metadata.clone(),
                )
                .await
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(ClientError::NetworkError(format!(
                    "initial connect attempt timed out after {budget:?}"
                ))),
            };
            match attempt_result {
                Ok(parts) => break parts,
                Err(err) => {
                    if attempt >= INITIAL_CONNECT_MAX_ATTEMPTS || !initial_connect_retryable(&err) {
                        return Err(err);
                    }
                    let wait = backoff_for(attempt, &reconnect_backoff, initial_jitter_seed, 0);
                    warn!(
                        url = %config.url,
                        attempt,
                        ?wait,
                        error = %err,
                        "initial connect attempt failed; retrying"
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        };
        *connection_id.lock().await = Some(ack.connection_id.clone());
        info!(
            url = %config.url,
            connection_id = %ack.connection_id,
            "server connection established"
        );
        let early_notif_rx = parking_lot::Mutex::new(match config.kind {
            ConnectionKind::ToolServer => Some(demux.subscribe_notifications()),
            _ => None,
        });
        let writer_error: WriteErrorSlot = Arc::new(parking_lot::Mutex::new(None));
        let inner = Arc::new(HubConnectionInner {
            key,
            kind: config.kind,
            credential: config.credential,
            on_reconnect: config.on_reconnect.clone(),
            on_disconnect: config.on_disconnect.clone(),
            on_terminal_close: config.on_terminal_close.clone(),
            server_id: config.server_id,
            server_description: config.server_description,
            server_metadata: config.server_metadata,
            alpha_test_key: config.alpha_test_key,
            allow_insecure_ws: config.allow_insecure_ws,
            on_fatal: config.on_fatal,
            reconnect_backoff,
            reconnect_jitter_seed: new_reconnect_jitter_seed(),
            attempt_reset_after,
            reconnect_after_terminal_close_codes: {
                let mut codes = config.tuning.reconnect_after_terminal_close_codes.clone();
                codes.sort_unstable();
                codes.dedup();
                codes
            },
            outage_seq: AtomicU32::new(0),
            outbound_tx,
            demux: demux.clone(),
            bound_sessions: bound_sessions.clone(),
            connection_id,
            hello_capabilities: parking_lot::RwLock::new(ack.capabilities),
            next_request_id: std::sync::atomic::AtomicU64::new(1),
            shutdown,
            stop_tx,
            reconnect_tx,
            early_notif_rx,
            health: ConnHealth::new(),
            writer_error: writer_error.clone(),
        });
        let (writer_ctl_tx, writer_ctl_rx) =
            mpsc::channel::<WriterControl<SplitSink<WsStream, Message>>>(WRITER_CTL_CAPACITY);
        let (writer_stop_tx, writer_stop_rx) = mpsc::channel::<()>(1);
        let (priority_tx, priority_rx) = mpsc::channel::<String>(PRIORITY_OUTBOUND_CAPACITY);
        let (writer_ready_tx, writer_ready_rx) = oneshot::channel();
        let writer_handle = tokio::spawn(run_writer(
            sink,
            outbound_rx,
            priority_rx,
            writer_ctl_rx,
            writer_stop_rx,
            Some(ws_ping_interval),
            writer_error,
            Some(writer_ready_tx),
        ));
        let _ = writer_ready_rx.await;
        if let Some(cb) = &config.on_connect {
            cb();
        }
        let reader_inner = inner.clone();
        tokio::spawn(run_reader_actor(
            reader_inner,
            stream,
            stop_rx,
            reconnect_rx,
            writer_ctl_tx,
            writer_stop_tx,
            writer_handle,
            priority_tx,
            config.url,
            ws_liveness_deadline,
        ));
        Ok(Arc::new(Self { inner }))
    }
    /// Pool dedup key for this connection.
    pub fn key(&self) -> &ConnKey {
        &self.inner.key
    }
    /// Connection role.
    pub fn kind(&self) -> ConnectionKind {
        self.inner.kind
    }
    /// Stable identity of this connection's actor state. Lets the pool
    /// evict by identity so a connection only ever forgets its own slot.
    pub(crate) fn actor_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as *const () as usize
    }
    /// Server-issued connection id of the most recently established
    /// (post-handshake) socket. During a reconnect gap this still names the
    /// dropped connection until the next handshake + replay completes.
    pub async fn connection_id(&self) -> Option<ConnectionId> {
        self.inner.connection_id.lock().await.clone()
    }
    /// Whether the server advertised `capability` (a wire method string,
    /// e.g. `"session_attach_server"`) in the CURRENT connection's
    /// `hello_ack`.
    ///
    /// - `Some(true)`: advertised.
    /// - `Some(false)`: the ack carried a non-empty capability list that
    ///   does not include it.
    /// - `None`: the ack advertised nothing — servers predating the
    ///   `capabilities` field are indistinguishable from an empty list, so
    ///   support is unknown and callers should probe per call.
    pub fn supports(&self, capability: &str) -> Option<bool> {
        let caps = self.inner.hello_capabilities.read();
        if caps.is_empty() {
            return None;
        }
        Some(caps.iter().any(|c| c == capability))
    }
    /// Demux (used by the server-side run loop to register session
    /// inboxes). Cheap to clone (Arc bump).
    pub fn demux(&self) -> Arc<Demux> {
        self.inner.demux.clone()
    }
    pub(crate) fn take_early_notifications(&self) -> Option<broadcast::Receiver<Value>> {
        self.inner.early_notif_rx.lock().take()
    }
    pub(crate) fn force_reconnect(&self) {
        let _ = self.inner.reconnect_tx.try_send(());
    }
    /// Future that resolves once the connection actor has shut down.
    pub async fn await_shutdown(&self) {
        self.inner.shutdown.cancelled().await;
    }
    /// Signal the connection actor to begin shutdown. The actor
    /// drains its in-flight waiters with `NetworkError` and exits;
    /// the outbound channel closes shortly after, so subsequent
    /// [`Self::send_outbound`] calls return
    /// [`ClientError::NetworkError`]. [`Self::await_shutdown`]
    /// resolves once the actor task has terminated.
    ///
    /// Idempotent: redundant calls are no-ops. Equivalent to
    /// dropping the last `Arc<HubConnection>`, but lets a holder
    /// trigger shutdown without giving up its reference.
    pub fn request_shutdown(&self) {
        let _ = self.inner.stop_tx.try_send(());
    }
    /// Increment the refcount on `session_id`. The session is tracked
    /// locally for reconnect-replay; the server learns about it via
    /// `serve` (auto-registration on the server side).
    pub fn track_session(&self, session_id: SessionId) {
        self.inner.bound_sessions.increment(session_id);
    }
    /// Decrement the refcount on `session_id`. Removes tracking when
    /// the last borrower drops. Returns the post-decrement count
    /// (`Some(0)` = last borrower; `None` = key was absent).
    pub fn untrack_session(&self, session_id: &SessionId) -> Option<u64> {
        self.inner.bound_sessions.decrement(session_id)
    }
    /// Send a JSON-RPC request and await the response.
    ///
    /// The waiter is registered before the frame is sent so a fast response
    /// can never arrive before its waiter exists, and is reclaimed on send
    /// failure (via [`WaiterGuard`]) so a call that never reached the wire
    /// cannot leak a parked waiter across a reconnect episode.
    pub async fn call_request<P>(
        &self,
        request_id: echo_agent_tool_protocol::RequestId,
        request: &JsonRpcRequest<P>,
    ) -> Result<JsonRpcResponse, ClientError>
    where
        P: serde::Serialize,
    {
        let text = serde_json::to_string(request)?;
        let (tx, rx) = oneshot::channel();
        self.inner
            .demux
            .register_response_waiter(request_id.clone(), tx);
        let _guard = WaiterGuard {
            demux: &self.inner.demux,
            request_id: &request_id,
        };
        self.send_outbound(text).await?;
        rx.await?
    }
    /// Send a JSON-RPC request and await the response, bounded by `timeout`.
    pub async fn call_request_with_timeout<P>(
        &self,
        request_id: echo_agent_tool_protocol::RequestId,
        request: &JsonRpcRequest<P>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, ClientError>
    where
        P: serde::Serialize,
    {
        self.call_request_with_deadline(request_id, request, timeout)
            .await
            .map_err(ClientError::from)
    }
    async fn call_request_with_deadline<P>(
        &self,
        request_id: echo_agent_tool_protocol::RequestId,
        request: &JsonRpcRequest<P>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, DeadlineCallError>
    where
        P: serde::Serialize,
    {
        let text =
            serde_json::to_string(request).map_err(|e| DeadlineCallError::Other(e.into()))?;
        let (tx, rx) = oneshot::channel();
        self.inner
            .demux
            .register_response_waiter(request_id.clone(), tx);
        let _guard = WaiterGuard {
            demux: &self.inner.demux,
            request_id: &request_id,
        };
        self.send_outbound(text)
            .await
            .map_err(DeadlineCallError::Other)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result.map_err(DeadlineCallError::Other),
            Ok(Err(recv_err)) => Err(DeadlineCallError::Other(recv_err.into())),
            Err(_elapsed) => Err(DeadlineCallError::TimedOut(timeout)),
        }
    }
    /// Send a fully-formed JSON text frame onto the outbound channel.
    /// Used by the server-side handler when replying to a
    /// `tool_call_request` (the response flows out without going
    /// through a waiter).
    pub async fn send_outbound(&self, text: String) -> Result<(), ClientError> {
        match self.inner.outbound_tx.try_send(text) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(text)) => {
                match tokio::time::timeout(
                    Duration::from_millis(250),
                    self.inner.outbound_tx.send(text),
                )
                .await
                {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err(ClientError::NetworkError(
                        "outbound channel closed".to_owned(),
                    )),
                    Err(_) => Err(ClientError::BackpressureError(
                        "outbound mpsc full beyond bounded wait".to_owned(),
                    )),
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientError::NetworkError(
                "outbound channel closed".to_owned(),
            )),
        }
    }
    /// Non-blocking enqueue for synchronous drop paths that cannot
    /// `.await` (e.g. `RemoteCallStream::Drop` cancel-on-drop). A full
    /// or closed channel returns `Err` and the caller abandons the
    /// frame — best-effort, mirroring the heartbeat-pong drop discipline.
    pub(crate) fn try_send_outbound(&self, text: String) -> Result<(), ClientError> {
        match self.inner.outbound_tx.try_send(text) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(ClientError::BackpressureError(
                "outbound mpsc full".to_owned(),
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientError::NetworkError(
                "outbound channel closed".to_owned(),
            )),
        }
    }
    /// Allocate a fresh request id. Monotonic per-connection.
    ///
    /// Returns `Err` only if a future-added `RequestId` invariant
    /// rejects the formatted `c{value}` string (today the only
    /// failure path is the empty-string check, which `format!` cannot
    /// produce). Callers in non-fallible contexts should propagate
    /// the error rather than panic.
    pub fn try_alloc_request_id(&self) -> Result<echo_agent_tool_protocol::RequestId, ClientError> {
        let value = self
            .inner
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        echo_agent_tool_protocol::RequestId::new(format!("c{value}")).map_err(ClientError::from)
    }
    /// Number of sessions currently bound to this connection.
    /// Stable observable for monitoring and tests; not on the hot path.
    pub fn bound_session_count(&self) -> usize {
        self.inner.bound_sessions.len()
    }
    /// Send a `serve` frame: full tool snapshot for a session.
    ///
    /// Idempotent: re-sending replaces the tool set. The server diffs
    /// against the previous snapshot and emits `tools_changed` to
    /// subscribed harnesses.
    pub async fn serve(
        &self,
        session_id: SessionId,
        params: echo_agent_tool_protocol::ServeParams,
    ) -> Result<echo_agent_tool_protocol::ServeResult, ClientError> {
        let mut last_err: Option<ClientError> = None;
        for attempt in 1..=SERVE_MAX_ATTEMPTS {
            let request_id = self.try_alloc_request_id()?;
            let req = JsonRpcRequest {
                jsonrpc: JsonRpcVersion,
                id: JsonRpcId::from_request_id(&request_id),
                session_id: Some(session_id.clone()),
                method: Method::Serve.as_wire_str().to_owned(),
                params: &params,
            };
            match self
                .call_request_with_deadline(request_id, &req, SERVE_ATTEMPT_TIMEOUT)
                .await
            {
                Ok(resp) => {
                    return match resp.outcome {
                        ResponseOutcome::Result(value) => serde_json::from_value(value)
                            .map_err(|e| ClientError::Serde(e.to_string())),
                        ResponseOutcome::Error(err) => Err(ClientError::from_jsonrpc_error(err)),
                    };
                }
                Err(DeadlineCallError::TimedOut(timeout)) => {
                    crate::metrics::serve_replay_timeout();
                    warn!(%session_id, attempt, ?timeout, "serve attempt timed out; will retry");
                    last_err = Some(DeadlineCallError::TimedOut(timeout).into());
                }
                Err(DeadlineCallError::Other(e)) => return Err(e),
            }
        }
        warn!(
            %session_id,
            attempts = SERVE_MAX_ATTEMPTS,
            "serve timed out every bounded attempt; forcing reconnect to restart replay"
        );
        self.force_reconnect();
        Err(last_err.unwrap_or_else(|| {
            ClientError::NetworkError("serve failed after bounded retries".to_owned())
        }))
    }
}
impl Drop for HubConnection {
    fn drop(&mut self) {
        let _ = self.inner.stop_tx.try_send(());
    }
}
/// True iff `url`'s host is one of the canonical loopback names. Case
/// insensitive on the hostname; IP literals match the standard loopback
/// addresses for IPv4 and IPv6.
pub(crate) fn host_is_loopback(url: &Url) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}
/// Open a fresh `ws://` / `wss://` socket. No handshake yet.
///
/// Refuses to send the credential over `ws://` to any non-loopback host
/// so the bearer token never crosses the network in plaintext. Local
/// loopback (`127.0.0.1`, `::1`, `localhost`) is the explicit exception
/// for development and local-proxy use; every other host must be
/// reached over `wss://`.
async fn open_socket(
    url: &Url,
    credential: &AuthCredential,
    kind: ConnectionKind,
    alpha_test_key: Option<&str>,
    allow_insecure_ws: bool,
) -> Result<WsStream, ClientError> {
    let is_plaintext_remote = url.scheme() != "wss" && !host_is_loopback(url);
    if is_plaintext_remote && !allow_insecure_ws {
        return Err(ClientError::InsecureScheme { url: url.clone() });
    }
    if is_plaintext_remote {
        warn!(
            host = %url.host_str().unwrap_or(""),
            "opening server connection over plaintext ws:// (allow_insecure_ws=true); bearer crosses the network in cleartext"
        );
    }
    let mut connect_url = url.clone();
    let expected_role = match kind {
        ConnectionKind::Harness => "harness",
        ConnectionKind::ToolServer => "tool_server",
    };
    if let Some(existing) = connect_url
        .query_pairs()
        .find(|(k, _)| k == "role")
        .map(|(_, v)| v.to_string())
    {
        if existing != expected_role {
            return Err(ClientError::InvalidConfig(format!(
                "URL query parameter role={existing} conflicts with ConnectionKind::{kind:?} (expected role={expected_role})"
            )));
        }
    } else {
        connect_url
            .query_pairs_mut()
            .append_pair("role", expected_role);
    }
    let mut request = connect_url
        .as_str()
        .into_client_request()
        .map_err(|e| ClientError::InvalidConfig(format!("invalid ws request: {e}")))?;
    let headers = request.headers_mut();
    for (name, value) in credential.upgrade_headers() {
        let header_name: HeaderName = name;
        let header_value: HeaderValue = HeaderValue::from_str(&value)
            .map_err(|e| ClientError::InvalidConfig(format!("invalid auth header value: {e}")))?;
        headers.insert(header_name, header_value);
    }
    let _ = alpha_test_key;
    echo_agent_tracing::http_client::attach_trace_to_http_request(headers);
    let (ws, _resp) = connect_async(request)
        .await
        .map_err(ClientError::from_handshake_error)?;
    Ok(ws)
}
/// Drive the hello / hello_ack exchange and hand back the (sink,
/// stream) pair for steady-state use.
async fn run_handshake(
    mut sink: SplitSink<WsStream, Message>,
    mut stream: SplitStream<WsStream>,
    kind: ConnectionKind,
    server_id: Option<echo_agent_tool_protocol::ServerId>,
    server_description: Option<String>,
    server_metadata: Option<serde_json::Value>,
) -> Result<
    (
        SplitSink<WsStream, Message>,
        SplitStream<WsStream>,
        echo_agent_tool_protocol::HelloAckMsg,
    ),
    ClientError,
> {
    let ack = send_hello(
        &mut sink,
        &mut stream,
        kind,
        server_id,
        server_description,
        server_metadata,
    )
    .await?;
    Ok((sink, stream, ack))
}
/// Outcome of the connected-phase loop.
enum ConnectedExit {
    /// Stop signal — actor terminates.
    Stop,
    /// Socket closed / errored — actor enters reconnect.
    SocketClosed(DisconnectCause),
    /// Server sent a close frame with a code that means "do not reconnect"
    /// (e.g. force eviction, session expired, admin disconnect).
    TerminalClose(u16),
}
/// Current Unix time in milliseconds (saturating to 0 before the epoch).
fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
enum InboundText {
    AppPing { pong: Option<String> },
    AppPong,
    Data,
    Unparseable,
}
fn classify_inbound_text(inner: &HubConnectionInner, text: &str) -> InboundText {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => match value.get("method").and_then(Value::as_str) {
            Some(m) if m == Method::Ping.as_wire_str() => InboundText::AppPing {
                pong: serde_json::to_string(&PongFrame::new(now_unix_millis())).ok(),
            },
            Some(m) if m == Method::Pong.as_wire_str() => InboundText::AppPong,
            _ => {
                let _ = inner.demux.route(value);
                InboundText::Data
            }
        },
        Err(e) => {
            warn!(?e, "discarding unparseable inbound text frame");
            InboundText::Unparseable
        }
    }
}
fn rearm_liveness(deadline: &mut std::pin::Pin<&mut tokio::time::Sleep>, liveness: Duration) {
    let now = tokio::time::Instant::now();
    let rearm = now
        .checked_add(liveness)
        .unwrap_or_else(|| now + Duration::from_secs(86400 * 365 * 30));
    deadline.as_mut().reset(rearm);
}
/// Terminal close code for a hibernated-but-restorable sandbox the hub
/// reaped; the only 4100–4199 code that is safe to reconnect after.
pub const CLOSE_CODE_SANDBOX_TERMINATED: u16 = 4103;
/// Map a websocket close frame's code to the connected-phase exit. Close
/// codes 4100-4199 are terminal by protocol contract (the server
/// intentionally ended the connection: eviction, session expiry, admin
/// disconnect, rate limit). The actor still stops on these unless the
/// embedder allowlisted the specific code via
/// [`ConnectionTuning::reconnect_after_terminal_close_codes`].
/// The range is deliberately wide so new terminal codes added server-side
/// are recognised without a client update.
fn exit_for_close_code(code: Option<u16>) -> ConnectedExit {
    match code {
        Some(code) if (4100..4200).contains(&code) => ConnectedExit::TerminalClose(code),
        _ => ConnectedExit::SocketClosed(DisconnectCause::CloseFrame(code)),
    }
}
/// Classify why the inbound stream ended, preferring a write error the
/// writer task recorded over what the reader observed.
///
/// Best-effort: the writer task populates `writer_error` asynchronously
/// after its send fails, so the reader can observe the resulting stream
/// EOF/error and classify it here *before* the slot is set. In that
/// (telemetry-only) race a genuine write-side failure is reported as
/// `eof` / `transport_read_error` instead of `transport_write_error`.
fn classify_stream_end(inner: &HubConnectionInner, read_error: Option<String>) -> DisconnectCause {
    if let Some(detail) = inner.writer_error.lock().take() {
        return DisconnectCause::WriteError(detail);
    }
    match read_error {
        Some(detail) => DisconnectCause::ReadError(detail),
        None => DisconnectCause::Eof,
    }
}
/// Control messages handed to the dedicated writer task.
///
/// The reader is the sole reconnect driver; it `Pause`s the writer the
/// instant the socket is known dead so no buffered frame is dequeued
/// onto the corpse, then `Resume`s it with the fresh sink once the
/// handshake completes. Carried on [`WRITER_CTL_CAPACITY`] so a liveness
/// `Close`+`Pause` cannot crowd out `Resume`.
enum WriterControl<S> {
    /// Socket is dead; stop draining `outbound_rx` (frames stay buffered).
    Pause,
    /// Reconnected; install the fresh sink and resume draining.
    Resume(S),
    /// Send a WS Close then stop draining (liveness kill / orderly drop).
    Close { code: u16, reason: String },
}
/// Outcome of racing a sink write against writer ctl/stop.
enum SendOrPreempt<S> {
    Sent(Result<(), String>),
    Ctl(WriterControl<S>),
    Stop,
}
async fn send_or_preempt<S>(
    sink: &mut S,
    msg: Message,
    writer_ctl_rx: &mut mpsc::Receiver<WriterControl<S>>,
    writer_stop_rx: &mut mpsc::Receiver<()>,
) -> SendOrPreempt<S>
where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    tokio::select! {
        biased;
        _ = writer_stop_rx.recv() => SendOrPreempt::Stop,
        ctl = writer_ctl_rx.recv() => match ctl {
            Some(ctl) => SendOrPreempt::Ctl(ctl),
            None => SendOrPreempt::Stop,
        },
        result = sink.send(msg) => SendOrPreempt::Sent(result.map_err(|e| e.to_string())),
    }
}
/// Dedicated writer task: owns the sink, drains `outbound_rx`, and fires
/// the keepalive ping (`ping_period`) — but only while `live`. Between a `Pause` and
/// the matching `Resume` it parks on the control/stop channels only, so
/// frames enqueued during the reconnect gap stay buffered in
/// `outbound_rx` and flush after `Resume`.
///
/// Data/ping writes are raced against ctl via [`send_or_preempt`] so a
/// half-open socket cannot strand Pause/Close/Resume behind `sink.send`.
/// Close is time-boxed against stop+timeout only — a queued Pause must
/// not abandon Close 1001.
async fn run_writer<S>(
    mut sink: S,
    mut outbound_rx: mpsc::Receiver<String>,
    mut priority_rx: mpsc::Receiver<String>,
    mut writer_ctl_rx: mpsc::Receiver<WriterControl<S>>,
    mut writer_stop_rx: mpsc::Receiver<()>,
    ping_period: Option<Duration>,
    write_error: WriteErrorSlot,
    ready: Option<oneshot::Sender<()>>,
) where
    S: futures::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let keepalive = ping_period.is_some();
    let mut ping_interval = tokio::time::interval(ping_period.unwrap_or(Duration::from_secs(3600)));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    if !keepalive {
        ping_interval.tick().await;
    }
    let mut live = true;
    let mut ready = ready;
    let mut pending_app_ping = false;
    loop {
        if let Some(tx) = ready.take() {
            let _ = tx.send(());
        }
        if live && pending_app_ping {
            pending_app_ping = false;
            let queued = match priority_rx.try_recv() {
                Ok(text) => Some(text),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    match outbound_rx.try_recv() {
                        Ok(text) => Some(text),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            break;
                        }
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            };
            let mut pending_ctl: Option<WriterControl<S>> = None;
            if let Some(text) = queued {
                match send_or_preempt(
                    &mut sink,
                    Message::Text(text.into()),
                    &mut writer_ctl_rx,
                    &mut writer_stop_rx,
                )
                .await
                {
                    SendOrPreempt::Stop => break,
                    SendOrPreempt::Ctl(ctl) => pending_ctl = Some(ctl),
                    SendOrPreempt::Sent(Err(e)) => {
                        *write_error.lock() = Some(format!("frame send failed: {e}"));
                        crate::metrics::writer_sink_send_error();
                        live = false;
                    }
                    SendOrPreempt::Sent(Ok(())) => {}
                }
            }
            if live
                && pending_ctl.is_none()
                && let Ok(text) = serde_json::to_string(&PingFrame::new(now_unix_millis()))
            {
                match send_or_preempt(
                    &mut sink,
                    Message::Text(text.into()),
                    &mut writer_ctl_rx,
                    &mut writer_stop_rx,
                )
                .await
                {
                    SendOrPreempt::Stop => break,
                    SendOrPreempt::Ctl(ctl) => pending_ctl = Some(ctl),
                    SendOrPreempt::Sent(Err(e)) => {
                        *write_error.lock() = Some(format!("app ping send failed: {e}"));
                        crate::metrics::writer_sink_send_error();
                        live = false;
                    }
                    SendOrPreempt::Sent(Ok(())) => {}
                }
            }
            while let Some(ctl) = pending_ctl.take() {
                match ctl {
                    WriterControl::Pause => {
                        live = false;
                        pending_app_ping = false;
                        while priority_rx.try_recv().is_ok() {}
                    }
                    WriterControl::Close { code, reason } => {
                        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
                        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
                        live = false;
                        pending_app_ping = false;
                        while priority_rx.try_recv().is_ok() {}
                        let close_msg = Message::Close(Some(CloseFrame {
                            code: CloseCode::from(code),
                            reason: reason.into(),
                        }));
                        tokio::select! {
                            biased;
                            _ = writer_stop_rx.recv() => return,
                            _ = tokio::time::sleep(WRITER_CLOSE_SEND_TIMEOUT) => {
                                *write_error.lock() =
                                    Some("close send timed out".to_owned());
                                crate::metrics::writer_sink_send_error();
                            }
                            result = sink.send(close_msg) => {
                                if let Err(e) = result {
                                    *write_error.lock() =
                                        Some(format!("close send failed: {e}"));
                                    crate::metrics::writer_sink_send_error();
                                }
                            }
                        }
                    }
                    WriterControl::Resume(new_sink) => {
                        sink = new_sink;
                        live = true;
                        pending_app_ping = false;
                        while priority_rx.try_recv().is_ok() {}
                        write_error.lock().take();
                        ping_interval =
                            tokio::time::interval(ping_period.unwrap_or(Duration::from_secs(3600)));
                        ping_interval
                            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        if !keepalive {
                            ping_interval.tick().await;
                        }
                    }
                }
            }
            continue;
        }
        let mut pending_ctl: Option<WriterControl<S>> = tokio::select! {
            biased;
            _ = writer_stop_rx.recv() => break,
            ctl = writer_ctl_rx.recv() => match ctl {
                Some(ctl) => Some(ctl),
                None => break,
            },
            _ = ping_interval.tick(), if live && keepalive && !pending_app_ping => {
                match send_or_preempt(
                    &mut sink,
                    Message::Ping(Vec::new().into()),
                    &mut writer_ctl_rx,
                    &mut writer_stop_rx,
                ).await {
                    SendOrPreempt::Stop => break,
                    SendOrPreempt::Ctl(ctl) => Some(ctl),
                    SendOrPreempt::Sent(Err(e)) => {
                        *write_error.lock() = Some(format!("ping send failed: {e}"));
                        crate::metrics::writer_sink_send_error();
                        live = false;
                        None
                    }
                    SendOrPreempt::Sent(Ok(())) => {
                        // App ping survives proxies that eat WS control frames.
                        pending_app_ping = true;
                        None
                    }
                }
            }
            priority = priority_rx.recv(), if live => match priority {
                Some(text) => match send_or_preempt(
                    &mut sink,
                    Message::Text(text.into()),
                    &mut writer_ctl_rx,
                    &mut writer_stop_rx,
                ).await {
                    SendOrPreempt::Stop => break,
                    SendOrPreempt::Ctl(ctl) => Some(ctl),
                    SendOrPreempt::Sent(Err(e)) => {
                        *write_error.lock() = Some(format!("priority send failed: {e}"));
                        crate::metrics::writer_sink_send_error();
                        live = false;
                        None
                    }
                    SendOrPreempt::Sent(Ok(())) => None,
                },
                None => break,
            },
            outbound = outbound_rx.recv(), if live => match outbound {
                Some(text) => match send_or_preempt(
                    &mut sink,
                    Message::Text(text.into()),
                    &mut writer_ctl_rx,
                    &mut writer_stop_rx,
                ).await {
                    SendOrPreempt::Stop => break,
                    SendOrPreempt::Ctl(ctl) => Some(ctl),
                    SendOrPreempt::Sent(Err(e)) => {
                        *write_error.lock() = Some(format!("frame send failed: {e}"));
                        crate::metrics::writer_sink_send_error();
                        live = false;
                        None
                    }
                    SendOrPreempt::Sent(Ok(())) => None,
                },
                None => break,
            },
        };
        while let Some(ctl) = pending_ctl.take() {
            match ctl {
                WriterControl::Pause => {
                    live = false;
                    pending_app_ping = false;
                    while priority_rx.try_recv().is_ok() {}
                }
                WriterControl::Close { code, reason } => {
                    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
                    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
                    live = false;
                    pending_app_ping = false;
                    while priority_rx.try_recv().is_ok() {}
                    let close_msg = Message::Close(Some(CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    }));
                    tokio::select! {
                        biased;
                        _ = writer_stop_rx.recv() => return,
                        _ = tokio::time::sleep(WRITER_CLOSE_SEND_TIMEOUT) => {
                            *write_error.lock() =
                                Some("close send timed out".to_owned());
                            crate::metrics::writer_sink_send_error();
                        }
                        result = sink.send(close_msg) => {
                            if let Err(e) = result {
                                *write_error.lock() =
                                    Some(format!("close send failed: {e}"));
                                crate::metrics::writer_sink_send_error();
                            }
                        }
                    }
                }
                WriterControl::Resume(new_sink) => {
                    sink = new_sink;
                    live = true;
                    pending_app_ping = false;
                    while priority_rx.try_recv().is_ok() {}
                    write_error.lock().take();
                    ping_interval =
                        tokio::time::interval(ping_period.unwrap_or(Duration::from_secs(3600)));
                    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    if !keepalive {
                        ping_interval.tick().await;
                    }
                }
            }
        }
    }
}
/// Invoke the optional disconnect callback (best-effort, sync).
fn fire_on_disconnect(inner: &HubConnectionInner) {
    if let Some(cb) = &inner.on_disconnect {
        cb();
    }
}
/// Invoke the optional terminal-close callback (best-effort, sync).
fn fire_on_terminal_close(inner: &HubConnectionInner, code: u16) {
    if let Some(cb) = &inner.on_terminal_close {
        cb(code);
    }
}
/// Reader half of the split actor: owns the stream, routes inbound
/// frames, and drives reconnect. Never touches the sink — it asks the
/// writer task to `Pause`/`Resume` instead.
async fn run_reader_actor(
    inner: Arc<HubConnectionInner>,
    mut stream: SplitStream<WsStream>,
    mut stop_rx: mpsc::Receiver<()>,
    mut reconnect_rx: mpsc::Receiver<()>,
    writer_ctl_tx: mpsc::Sender<WriterControl<SplitSink<WsStream, Message>>>,
    writer_stop_tx: mpsc::Sender<()>,
    writer_handle: tokio::task::JoinHandle<()>,
    priority_tx: mpsc::Sender<String>,
    url: Url,
    liveness_deadline: Duration,
) {
    let mut attempt: u32 = 0;
    let mut connected_at = Instant::now();
    'actor: loop {
        match run_reader_phase(
            inner.as_ref(),
            &mut stream,
            &mut stop_rx,
            &mut reconnect_rx,
            liveness_deadline,
            &priority_tx,
        )
        .await
        {
            ConnectedExit::Stop => break,
            ConnectedExit::TerminalClose(code)
                if inner
                    .reconnect_after_terminal_close_codes
                    .binary_search(&code)
                    .is_err() =>
            {
                info!(code, url = %url, "server sent terminal close; not reconnecting");
                fire_on_terminal_close(inner.as_ref(), code);
                fire_on_disconnect(inner.as_ref());
                inner.demux.drain_waiters_with(|| {
                    ClientError::Closed(format!("server terminal close (code {code})"))
                });
                inner.demux.drain_progress();
                break;
            }
            exit => {
                let (cause, already_notified) = match exit {
                    ConnectedExit::Stop => {
                        unreachable!("Stop is handled by the arm above")
                    }
                    ConnectedExit::TerminalClose(code) => {
                        info!(
                            code,
                            url = %url,
                            "server sent terminal close; reconnecting (embedder opt-in)"
                        );
                        fire_on_terminal_close(inner.as_ref(), code);
                        fire_on_disconnect(inner.as_ref());
                        inner.demux.drain_waiters_with(|| {
                            ClientError::Closed(format!("server terminal close (code {code})"))
                        });
                        inner.demux.drain_progress();
                        (DisconnectCause::CloseFrame(Some(code)), true)
                    }
                    ConnectedExit::SocketClosed(cause) => (cause, false),
                };
                let detected_at = Instant::now();
                let prev_conn_age = detected_at.duration_since(connected_at);
                let health = inner.health.snapshot();
                let prev_connection_id = inner.connection_id.lock().await.clone();
                let outage = OutageInfo {
                    prev_connection_id,
                    prev_connection_duration_ms: prev_conn_age.as_millis() as u64,
                    last_inbound: health.last_inbound,
                    detect_ms: detected_at.duration_since(health.last_inbound).as_millis() as u64,
                    since_last_probe_monotonic_ms: health.since_last_probe_monotonic_ms,
                    since_last_probe_wall_ms: health.since_last_probe_wall_ms,
                    clock_jump_ms: health.clock_jump_ms,
                    cause,
                };
                warn!(
                    url = %url,
                    cause = outage.cause.label(),
                    close_code = ?outage.cause.close_code(),
                    error_detail = ?outage.cause.detail(),
                    connection_id = ?outage.prev_connection_id,
                    prev_connection_duration_ms = outage.prev_connection_duration_ms,
                    detect_ms = outage.detect_ms,
                    since_last_probe_monotonic_ms = outage.since_last_probe_monotonic_ms,
                    since_last_probe_wall_ms = outage.since_last_probe_wall_ms,
                    clock_jump_ms = outage.clock_jump_ms,
                    "server connection lost; scheduling reconnect"
                );
                if !already_notified {
                    fire_on_disconnect(inner.as_ref());
                }
                if matches!(outage.cause, DisconnectCause::LivenessDeadline)
                    && writer_ctl_tx
                        .send(WriterControl::Close {
                            code: 1001,
                            reason: "liveness_deadline".to_owned(),
                        })
                        .await
                        .is_err()
                {
                    break;
                }
                if writer_ctl_tx.send(WriterControl::Pause).await.is_err() {
                    break;
                }
                inner.demux.drain_waiters_with(|| {
                    ClientError::NetworkError("socket dropped during in-flight call".to_owned())
                });
                inner.demux.drain_progress();
                if prev_conn_age >= inner.attempt_reset_after {
                    attempt = 0;
                }
                inner.begin_reconnect_outage();
                let mut backoff_total = Duration::ZERO;
                loop {
                    attempt = attempt.saturating_add(1);
                    let backoff = inner.reconnect_delay(attempt);
                    info!(?backoff, attempt, url = %url, "reconnecting server connection");
                    tokio::select! {
                        _ = stop_rx.recv() => break 'actor,
                        _ = sleep(backoff) => {}
                    }
                    backoff_total += backoff;
                    let reconnect_start = std::time::Instant::now();
                    let attempt_budget = reconnect_attempt_budget(liveness_deadline);
                    let outcome = tokio::select! {
                        _ = stop_rx.recv() => break 'actor,
                        outcome = tokio::time::timeout(
                            attempt_budget,
                            reconnect_and_replay(
                                inner.as_ref(),
                                &url,
                                attempt,
                                &outage,
                                backoff_total,
                            ),
                        ) => outcome.unwrap_or_else(|_elapsed| {
                            Err(ClientError::NetworkError(format!(
                                "reconnect attempt timed out after {attempt_budget:?}"
                            )))
                        }),
                    };
                    match outcome {
                        Ok((new_sink, new_stream)) => {
                            let elapsed = reconnect_start.elapsed().as_secs_f64();
                            crate::metrics::reconnect_succeeded();
                            crate::metrics::reconnect_duration_observe(elapsed);
                            inner.health.reset();
                            inner.writer_error.lock().take();
                            connected_at = Instant::now();
                            drain_reconnect_signals(&mut reconnect_rx);
                            stream = new_stream;
                            if writer_ctl_tx
                                .send(WriterControl::Resume(new_sink))
                                .await
                                .is_err()
                            {
                                break 'actor;
                            }
                            crate::metrics::reconnect_writer_resume();
                            break;
                        }
                        Err(ClientError::HandshakeAuthFailed { status }) => {
                            warn!(
                                status,
                                attempt,
                                "reconnect rejected with handshake auth failure; evicting pool entry and stopping"
                            );
                            crate::metrics::reconnect_failed("handshake_auth");
                            inner.demux.drain_waiters_with(|| {
                                ClientError::AuthError(format!(
                                    "server rejected reconnect handshake (HTTP {status})"
                                ))
                            });
                            inner.demux.drain_progress();
                            if let Some(pool) = inner.on_fatal.as_ref().and_then(Weak::upgrade) {
                                let own_id = Arc::as_ptr(&inner) as *const () as usize;
                                pool.forget_if(&inner.key, move |conn| conn.actor_id() == own_id);
                            }
                            break 'actor;
                        }
                        Err(err) => {
                            crate::metrics::reconnect_failed("transport");
                            warn!(
                                ?err,
                                attempt,
                                cause = outage.cause.label(),
                                backoff_total_ms = backoff_total.as_millis() as u64,
                                "reconnect attempt failed; will retry"
                            );
                        }
                    }
                }
            }
        }
    }
    inner
        .demux
        .drain_waiters_with(|| ClientError::NetworkError("connection actor exited".to_owned()));
    inner.demux.drain_progress();
    let _ = writer_stop_tx.send(()).await;
    drop(writer_ctl_tx);
    drop(stop_rx);
    drop(stream);
    if let Err(e) = writer_handle.await {
        warn!(?e, "writer task panicked during shutdown");
    }
    inner.shutdown.cancel();
}
fn drain_reconnect_signals(reconnect_rx: &mut mpsc::Receiver<()>) {
    while reconnect_rx.try_recv().is_ok() {}
}
/// Reader-only steady-state loop for the split actor: drives the inbound
/// half but never writes (app-level pongs route through `outbound_tx`; WS
/// pings are auto-answered by tungstenite on poll).
///
/// Enforces the inbound-liveness deadline: no *round-trip* proof (WS/app
/// pong) for the deadline window (default 4× the ping
/// cadence, see [`resolve_ws_liveness_deadline`]) means the return path is
/// silently dead, so exit via [`ConnectedExit::SocketClosed`] onto the
/// normal reconnect path. Hub app/WS pings alone do not re-arm. The
/// deadline runs only in this phase and re-arms on every (re)entry.
///
/// Generic over the stream for in-memory unit tests, mirroring
/// [`run_writer`].
async fn run_reader_phase<S>(
    inner: &HubConnectionInner,
    stream: &mut S,
    stop_rx: &mut mpsc::Receiver<()>,
    reconnect_rx: &mut mpsc::Receiver<()>,
    liveness_deadline: Duration,
    pong_tx: &mpsc::Sender<String>,
) -> ConnectedExit
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut clock_probe = tokio::time::interval(CLOCK_PROBE_INTERVAL);
    clock_probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    clock_probe.tick().await;
    let deadline = sleep(liveness_deadline);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            _ = stop_rx.recv() => return ConnectedExit::Stop,
            _ = reconnect_rx.recv() => {
                info!("forced reconnect requested; dropping current socket");
                return ConnectedExit::SocketClosed(DisconnectCause::Forced);
            }
            // Before the deadline arm so a frame that raced the expiry
            // proves liveness and wins.
            msg = stream.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Text(text) => {
                                match classify_inbound_text(inner, text.as_ref()) {
                                    InboundText::AppPing { pong } => {
                                        // Hub→client ping is not RTT proof. Reply if we
                                        // can; a dropped pong must not re-arm either.
                                        if let Some(pong_text) = pong
                                            && pong_tx.try_send(pong_text).is_err()
                                        {
                                            crate::metrics::heartbeat_pong_dropped();
                                        }
                                    }
                                    InboundText::AppPong => {
                                        inner.health.record_inbound();
                                        rearm_liveness(&mut deadline, liveness_deadline);
                                    }
                                    InboundText::Data => {
                                        // Hub→client data is one-way, not RTT proof.
                                    }
                                    InboundText::Unparseable => {}
                                }
                            }
                            // WS Pong is RTT proof of our Ping. Inbound WS Ping is
                            // auto-answered by tungstenite and is hub→client only.
                            Message::Pong(_) => {
                                inner.health.record_inbound();
                                rearm_liveness(&mut deadline, liveness_deadline);
                            }
                            Message::Ping(_) | Message::Frame(_) => {}
                            Message::Binary(_) => {
                                warn!("server sent binary frame; ignoring");
                            }
                            Message::Close(frame) => {
                                return exit_for_close_code(frame.map(|f| f.code.into()));
                            }
                        }
                    }
                    Some(Err(e)) => {
                        return ConnectedExit::SocketClosed(classify_stream_end(
                            inner,
                            Some(e.to_string()),
                        ));
                    }
                    None => {
                        return ConnectedExit::SocketClosed(classify_stream_end(inner, None));
                    }
                }
            }
            _ = clock_probe.tick() => inner.health.refresh_clock(),
            _ = &mut deadline => {
                crate::metrics::liveness_deadline_expired();
                warn!(
                    ?liveness_deadline,
                    "no RTT proof (WS/app pong) within the liveness deadline; declaring the socket dead and reconnecting"
                );
                return ConnectedExit::SocketClosed(DisconnectCause::LivenessDeadline);
            }
        }
    }
}
/// Reconnect once and replay every session binding + tool registration.
async fn reconnect_and_replay(
    inner: &HubConnectionInner,
    url: &Url,
    attempt: u32,
    outage: &OutageInfo,
    backoff_total: Duration,
) -> Result<(SplitSink<WsStream, Message>, SplitStream<WsStream>), ClientError> {
    let fresh_cred = inner.credential.current();
    let ws = open_socket(
        url,
        &fresh_cred,
        inner.kind,
        inner.alpha_test_key.as_deref(),
        inner.allow_insecure_ws,
    )
    .await?;
    let (sink, stream) = ws.split();
    let (mut sink, mut stream, mut ack) = run_handshake(
        sink,
        stream,
        inner.kind,
        inner.server_id.clone(),
        inner.server_description.clone(),
        inner.server_metadata.clone(),
    )
    .await?;
    let sessions = inner.bound_sessions.snapshot_keys();
    if inner.kind == ConnectionKind::Harness {
        for sid in &sessions {
            let req = echo_agent_tool_protocol::JsonRpcRequest {
                jsonrpc: echo_agent_tool_protocol::JsonRpcVersion,
                id: echo_agent_tool_protocol::JsonRpcId::new_uuid_v7(),
                session_id: Some(sid.clone()),
                method: Method::SessionOpen.as_wire_str().to_owned(),
                params: echo_agent_tool_protocol::SessionOpenParams {
                    resume: false,
                    last_seq: None,
                },
            };
            if let Ok(text) = serde_json::to_string(&req) {
                let _ = SinkExt::send(&mut sink, Message::Text(text.into())).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), StreamExt::next(&mut stream))
                    .await;
            }
        }
    }
    let sessions_replayed = sessions.len();
    let silent_gap_ms = outage.last_inbound.elapsed().as_millis() as u64;
    info!(
        attempt,
        sessions_replayed,
        cause = outage.cause.label(),
        close_code = ?outage.cause.close_code(),
        error_detail = ?outage.cause.detail(),
        prev_connection_id = ?outage.prev_connection_id,
        connection_id = %ack.connection_id,
        prev_connection_duration_ms = outage.prev_connection_duration_ms,
        silent_gap_ms,
        detect_ms = outage.detect_ms,
        backoff_total_ms = backoff_total.as_millis() as u64,
        since_last_probe_monotonic_ms = outage.since_last_probe_monotonic_ms,
        since_last_probe_wall_ms = outage.since_last_probe_wall_ms,
        clock_jump_ms = outage.clock_jump_ms,
        "server reconnect succeeded"
    );
    crate::metrics::reconnect_cause(outage.cause.label());
    if let Some(detail_class) = outage.cause.detail_class() {
        crate::metrics::disconnect_detail_class(outage.cause.label(), detail_class);
    }
    crate::metrics::reconnect_gap_observe(silent_gap_ms as f64 / 1_000.0);
    *inner.connection_id.lock().await = Some(ack.connection_id.clone());
    *inner.hello_capabilities.write() = std::mem::take(&mut ack.capabilities);
    if let Some(cb) = &inner.on_reconnect {
        cb(ReconnectEvent {
            connection_id: ack.connection_id,
            sessions_replayed,
            attempt,
        });
    }
    Ok((sink, stream))
}
impl HubConnectionInner {
    fn begin_reconnect_outage(&self) {
        self.outage_seq.fetch_add(1, Ordering::Relaxed);
    }
    fn reconnect_delay(&self, attempt: u32) -> Duration {
        backoff_for(
            attempt,
            &self.reconnect_backoff,
            self.reconnect_jitter_seed,
            self.outage_seq.load(Ordering::Relaxed),
        )
    }
}
/// Look up the slot for `attempt`, take
/// `window = min(cap, max(slot, SPREAD_FLOOR))`, then `Uniform(0, window)`.
/// Empty schedule → `Duration::ZERO`.
fn backoff_for(attempt: u32, schedule: &[Duration], jitter_seed: u64, outage: u32) -> Duration {
    let Some(&cap) = schedule.last() else {
        return Duration::ZERO;
    };
    let idx = (attempt as usize)
        .saturating_sub(1)
        .min(schedule.len().saturating_sub(1));
    let base = schedule.get(idx).copied().unwrap_or_default();
    apply_reconnect_jitter(
        backoff_window(base, cap),
        jitter_roll(jitter_seed, attempt, outage),
    )
}
fn duration_nanos_u64(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}
/// SplitMix64. Avalanches so nearby seeds / attempts produce uncorrelated rolls.
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
fn jitter_roll(seed: u64, attempt: u32, outage: u32) -> u64 {
    splitmix64(
        seed ^ u64::from(attempt).wrapping_mul(0xD1B5_4A32_D192_ED03)
            ^ u64::from(outage).wrapping_mul(0x9E37_79B9_7F4A_7C15),
    )
}
fn derive_jitter_seed(counter: u64, pid: u64, nanos: u64) -> u64 {
    splitmix64(
        nanos
            .wrapping_add(pid.wrapping_shl(32))
            .wrapping_add(counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    )
}
fn new_reconnect_jitter_seed() -> u64 {
    let n = NEXT_RECONNECT_JITTER_SEED.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    derive_jitter_seed(n, pid, nanos)
}
/// `min(cap, max(base, RECONNECT_SPREAD_FLOOR))`. Production and tests
/// share [`RECONNECT_SPREAD_FLOOR`]; tests also assert that value equals 1s.
fn backoff_window(base: Duration, cap: Duration) -> Duration {
    base.max(RECONNECT_SPREAD_FLOOR).min(cap)
}
/// Uniform in `[0, window)`. `window == 0` → zero. Never returns `window`
/// itself, so the documented cap is a hard exclusive ceiling when
/// `window == cap` — no Dirac pile-up at 10 s.
fn apply_reconnect_jitter(window: Duration, roll: u64) -> Duration {
    let window_ns = duration_nanos_u64(window);
    if window_ns == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(roll % window_ns)
}
#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
