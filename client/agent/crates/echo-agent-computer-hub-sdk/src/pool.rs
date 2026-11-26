//! Process-wide connection pool keyed by `(url, principal)`.
//!
//! Two [`crate::ToolServer`] builds with the same `(url, credential)`
//! observe the same `Arc<HubConnection>`; distinct credentials open
//! distinct sockets. The pool is the canonical entry point — direct
//! [`crate::HubConnection::connect`] calls are reserved for tests and
//! one-shot programs that explicitly want unpooled behaviour.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use echo_agent_tool_protocol::ConnectionKind;
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use url::Url;

use crate::auth::AuthProvider;
use crate::connection::{
    ConnKey, ConnectCallback, ConnectionConfig, ConnectionTuning, DisconnectCallback,
    HubConnection, ReconnectCallback, TerminalCloseCallback,
};
use crate::error::ClientError;

/// Idle window for the reaper: a pooled connection is evictable once it is
/// unused (`Arc::strong_count == 1`, i.e. only the pool holds it) **and**
/// `now - last_handout >= DEFAULT_POOL_IDLE_TTL`.
///
/// Note the clock is `last_handout` (the last time the pool returned the
/// connection), not the moment the last consumer `Arc` was dropped: a
/// connection held longer than the TTL and then released is eligible on the
/// very next sweep, with no extra post-drop grace period. The only hard
/// guarantee is that an in-use connection (`strong_count > 1`) is never
/// reaped. Tuned well above the server's own 90s dead-peer idle timeout so a
/// short borrow between turns of an active conversation isn't churned.
pub const DEFAULT_POOL_IDLE_TTL: Duration = Duration::from_secs(300);

/// How often the shared pool's idle reaper scans for evictable entries.
pub const DEFAULT_POOL_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// A pooled connection plus the last time it was handed out to a caller.
///
/// `last_handout` is refreshed on every [`HubConnectionPool::get_or_connect`]
/// hit (and on the initial insert), so a connection that is repeatedly
/// re-fetched never looks idle even if its [`Arc`] strong count briefly
/// returns to 1 between fetches. Eviction additionally requires
/// `Arc::strong_count == 1` (only the pool holds it), so a connection a
/// consumer still holds is never reaped regardless of `last_handout`.
struct Pooled {
    conn: Arc<HubConnection>,
    last_handout: Instant,
}

/// The process-global pool used by [`HubConnectionPool::shared`].
///
/// `tokio::sync::OnceCell` is preferred over `std::sync::OnceLock` /
/// `LazyLock` here because the pool is only ever observed from
/// async contexts (the connection actor lives on a tokio runtime
/// already), so the async-aware `get_or_init` semantics avoid the
/// blocking-init footgun of the sync alternatives without taking a
/// hard dependency on additional sync primitives.
///
/// Tests MUST use [`HubConnectionPool::new`] to avoid cross-test
/// pollution: cargo runs all integration tests in the same binary
/// unless otherwise configured, so any test that touches
/// `HubConnectionPool::shared()` leaves the pool populated for
/// subsequent tests.
static SHARED: OnceCell<Arc<HubConnectionPool>> = OnceCell::const_new();

/// Pool of live server connections.
pub struct HubConnectionPool {
    connections: DashMap<ConnKey, Pooled>,
}

impl std::fmt::Debug for HubConnectionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubConnectionPool")
            .field("connection_count", &self.connections.len())
            .finish()
    }
}

impl HubConnectionPool {
    /// Build a fresh, unshared pool. Tests typically use this so each
    /// test sees an isolated registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            connections: DashMap::new(),
        })
    }

    /// Return the process-wide shared pool, lazily initialising it on
    /// the first call. Subsequent callers in the same process observe
    /// the same `Arc`.
    ///
    /// The shared pool spawns an idle reaper (see [`Self::spawn_idle_reaper`])
    /// exactly once, so a connection that is unused (`strong_count == 1`) and
    /// has not been handed out for [`DEFAULT_POOL_IDLE_TTL`] is closed instead
    /// of living for the whole process lifetime. (Unpooled / test pools built
    /// via [`Self::new`] do not
    /// get a reaper; they can call [`Self::sweep_idle`] directly.)
    pub async fn shared() -> Arc<Self> {
        SHARED
            .get_or_init(|| async {
                let pool = Self::new();
                pool.spawn_idle_reaper(DEFAULT_POOL_IDLE_TTL, DEFAULT_POOL_SWEEP_INTERVAL);
                pool
            })
            .await
            .clone()
    }

    /// Look up an existing pooled connection for `(url, credential)`,
    /// or open a fresh one if no pooled entry exists.
    ///
    /// `kind` is the connection role announced in the hello frame. The
    /// pool is keyed by `(url, principal)` only; mixing
    /// [`ConnectionKind`] values for the same `(url, principal)` is a
    /// caller error and surfaces as a [`ClientError::InvalidConfig`].
    ///
    /// The optional extra access key is not part of the pool key, so the first
    /// caller's key is the one carried on a shared connection's handshake (in
    /// practice it is a per-deployment constant). The plaintext-scheme guard is
    /// re-checked on every call below so it can't be bypassed by a cached
    /// insecure entry.
    pub async fn get_or_connect(
        self: &Arc<Self>,
        url: Url,
        credential: Arc<dyn AuthProvider>,
        kind: ConnectionKind,
        on_reconnect: Option<Arc<ReconnectCallback>>,
        on_disconnect: Option<Arc<DisconnectCallback>>,
        server_id: Option<echo_agent_tool_protocol::ServerId>,
        alpha_test_key: Option<String>,
        allow_insecure_ws: bool,
    ) -> Result<Arc<HubConnection>, ClientError> {
        self.get_or_connect_tuned(
            url,
            credential,
            kind,
            on_reconnect,
            on_disconnect,
            None, // on_connect (unused by the simple wrapper)
            None, // on_terminal_close (unused by the simple wrapper)
            server_id,
            None,
            None,
            alpha_test_key,
            allow_insecure_ws,
            ConnectionTuning::default(),
        )
        .await
    }

    /// Like [`Self::get_or_connect`] but carries optional connection-tuning
    /// knobs ([`ConnectionTuning`]) onto a freshly-opened connection. A
    /// `Default` tuning is behaviourally identical to `get_or_connect`, so
    /// existing callers are unaffected.
    ///
    /// Tuning binds to the socket at open time: it takes effect only when
    /// THIS call opens the connection. Because the pool dedups by
    /// `(url, principal)`, a hit on an existing entry returns that
    /// connection as-is and the `tuning` argument is ignored — the first
    /// opener's ping/backoff settings win for the lifetime of the pooled
    /// connection. Callers that need distinct tuning must use a distinct
    /// `(url, principal)` or an unpooled [`HubConnection::connect`].
    pub(crate) async fn get_or_connect_tuned(
        self: &Arc<Self>,
        url: Url,
        credential: Arc<dyn AuthProvider>,
        kind: ConnectionKind,
        on_reconnect: Option<Arc<ReconnectCallback>>,
        on_disconnect: Option<Arc<DisconnectCallback>>,
        on_connect: Option<Arc<ConnectCallback>>,
        on_terminal_close: Option<Arc<TerminalCloseCallback>>,
        server_id: Option<echo_agent_tool_protocol::ServerId>,
        server_description: Option<String>,
        server_metadata: Option<serde_json::Value>,
        alpha_test_key: Option<String>,
        allow_insecure_ws: bool,
        tuning: ConnectionTuning,
    ) -> Result<Arc<HubConnection>, ClientError> {
        if url.scheme() != "wss" && !crate::connection::host_is_loopback(&url) && !allow_insecure_ws
        {
            return Err(ClientError::InsecureScheme { url });
        }
        let key = ConnKey {
            url: url.as_str().to_owned(),
            principal: credential.principal_key(),
        };
        if let Some(mut existing) = self.connections.get_mut(&key) {
            existing.last_handout = Instant::now();
            let conn = existing.conn.clone();
            drop(existing);
            if conn.kind() != kind {
                return Err(ClientError::InvalidConfig(format!(
                    "pool entry for {} bound to {:?}; rebuild requested {:?}",
                    key.url,
                    conn.kind(),
                    kind
                )));
            }
            return Ok(conn);
        }
        let config = ConnectionConfig {
            url,
            credential,
            kind,
            on_reconnect,
            on_disconnect,
            on_terminal_close,
            on_connect,
            server_id,
            server_description,
            server_metadata,
            outbound_buffer: None,
            tuning,
            alpha_test_key,
            allow_insecure_ws,
            on_fatal: Some(Arc::downgrade(self)),
        };
        let conn = HubConnection::connect(config).await?;
        // Race window: another caller may have inserted between our
        // `get` and `connect`. Resolve via `entry().or_insert_with`
        // semantics — if we lose the race we drop our fresh
        // connection and adopt the winning one.
        match self.connections.entry(key.clone()) {
            dashmap::Entry::Occupied(mut existing) => {
                existing.get_mut().last_handout = Instant::now();
                let winner = existing.get().conn.clone();
                drop(conn);
                if winner.kind() != kind {
                    return Err(ClientError::InvalidConfig(format!(
                        "pool entry for {} bound to {:?}; rebuild requested {:?}",
                        key.url,
                        winner.kind(),
                        kind
                    )));
                }
                Ok(winner)
            }
            dashmap::Entry::Vacant(slot) => {
                crate::metrics::pool_connections_inc();
                slot.insert(Pooled {
                    conn: conn.clone(),
                    last_handout: Instant::now(),
                });
                Ok(conn)
            }
        }
    }

    /// Number of pooled connections. Intended for tests and metrics.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// `true` when no connection is pooled.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// Forget the pooled connection for `key`. The actual underlying
    /// `Arc<HubConnection>` is dropped only when no other holder
    /// keeps a reference; the next [`Self::get_or_connect`] for the
    /// same key opens a fresh socket.
    pub fn forget(&self, key: &ConnKey) {
        if self.connections.remove(key).is_some() {
            crate::metrics::pool_connections_dec();
        }
    }

    /// Close and remove every pooled connection that is BOTH unused (no live
    /// consumer holds an `Arc` — only the pool does, so `strong_count == 1`)
    /// AND idle longer than `idle_ttl` (no hand-out within the window).
    /// Removing the entry drops the pool's last `Arc<HubConnection>`, whose
    /// `Drop` closes the socket.
    ///
    /// The strong-count check runs inside the map's per-shard lock (via
    /// [`DashMap::retain`]), serialised against `get_or_connect`, so a
    /// connection handed out concurrently is never evicted out from under a
    /// caller. Returns the number of connections evicted.
    pub fn sweep_idle(&self, idle_ttl: Duration) -> usize {
        let now = Instant::now();
        let mut evicted = 0usize;
        self.connections.retain(|_key, pooled| {
            let idle_for = now.saturating_duration_since(pooled.last_handout);
            // `strong_count == 1` ⇒ only this pool entry references the
            // connection, so no consumer can still be using it.
            let unused = Arc::strong_count(&pooled.conn) == 1;
            let evict = unused && idle_for >= idle_ttl;
            if evict {
                evicted += 1;
            }
            !evict
        });
        for _ in 0..evicted {
            crate::metrics::pool_connections_dec();
            crate::metrics::pool_evictions_inc();
        }
        evicted
    }

    /// Spawn a background task that calls [`Self::sweep_idle`] every
    /// `sweep_interval`, closing connections idle longer than `idle_ttl`.
    ///
    /// The task holds a [`std::sync::Weak`] to the pool, so it exits on its
    /// own once the last strong `Arc<HubConnectionPool>` is dropped (it never
    /// keeps the pool alive). The first interval tick is skipped so a
    /// freshly-handed-out connection is never swept on the immediate tick.
    pub fn spawn_idle_reaper(
        self: &Arc<Self>,
        idle_ttl: Duration,
        sweep_interval: Duration,
    ) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(sweep_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // `interval`'s first tick resolves immediately; skip it.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(pool) = weak.upgrade() else { break };
                pool.sweep_idle(idle_ttl);
            }
        })
    }

    /// Like [`Self::forget`] but identity-checked: only removes the slot
    /// when `predicate` accepts the currently-stored connection. The
    /// self-evicting actor passes an `Arc::ptr_eq` check so a race-loser
    /// can never drop the winner's fresh entry (ABA-safe).
    pub(crate) fn forget_if(
        &self,
        key: &ConnKey,
        predicate: impl FnOnce(&Arc<HubConnection>) -> bool,
    ) {
        if self
            .connections
            .remove_if(key, |_, pooled| predicate(&pooled.conn))
            .is_some()
        {
            crate::metrics::pool_connections_dec();
        }
    }
}
