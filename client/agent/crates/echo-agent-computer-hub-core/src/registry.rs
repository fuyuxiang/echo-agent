//! Object-safe `ToolRegistry` trait shared by every storage plane.
//!
//! Two registry implementations are expected: one in-memory plane for
//! statically-registered local tools, and one connection-keyed plane fed by
//! incoming remote registrations. Both expose the same trait so the
//! router can compose them through [`crate::CompoundResolver`] without
//! caring which is which.
//!
//! Mutations are connection-scoped: each registered tool belongs to the
//! [`ConnectionId`] that introduced it. Per-tool session bindings live
//! alongside the tool's record and are mutated independently via
//! [`ToolRegistry::bind_tool_session`] / [`ToolRegistry::unbind_tool_session`].
//! Reads (`find_tool`, `list_tools`, `search`) remain session-scoped — the
//! router resolves a tool by `(session_id, tool_id)`, never by
//! connection id.
//!
//! The concrete in-memory implementation is intentionally **out of scope**
//! for this crate — it requires a concurrency story (sharded maps, an
//! actor, etc.) that belongs alongside the registry's collision matrix and
//! generation handling. Tests exercise the trait via per-test mock impls.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use echo_agent_tool_protocol::{
    ConnectionId, RegistrationOutcome, ServerId, SessionId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, UserId,
};
use echo_agent_tool_runtime::{SearchSnapshot, ServerSummary};
use echo_agent_tool_types::ToolDescription;

use crate::resolver::ResolvedTool;

/// Outcome of a single [`ToolRegistry::bind_tool_session`] call.
///
/// This enum is the source of truth for storage outcomes; the wire enum
/// [`echo_agent_tool_protocol::ToolSessionBindOutcome`] is a strict subset with
/// one extra wire-only variant. The two layers diverge deliberately:
///
/// - `Conflict` (cross-connection race on the `(session_id, tool_id)`
///   reverse-index slot) is registry-internal: the router lifts it to a
///   top-level `ServerError::ToolBindingConflict` (-32600) instead of
///   mirroring it to the wire ack, so the contended caller gets a
///   dedicated error code rather than overloading `UnknownTool`.
/// - The wire enum's `SessionNotBound` is router-injected by the
///   per-frame envelope pre-check (the connection's bound-session set
///   lives in router state, not the registry) and is never produced by
///   any registry call — so it has no counterpart here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSessionBindOutcome {
    /// Added to the tool's session set.
    Bound,
    /// Session id was already in the tool's session set; no-op.
    AlreadyBound,
    /// No tool with the given id is registered against this connection.
    UnknownTool,
    /// Cross-connection conflict: another connection already holds the
    /// `(session_id, tool_id)` reverse-index slot. The router lifts this
    /// into a top-level `ToolBindingConflict` server error so the wire
    /// reply uses the dedicated -32600 code instead of the structurally
    /// dishonest `UnknownTool`. No registry state was mutated.
    Conflict,
}

/// Outcome of a single [`ToolRegistry::unbind_tool_session`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSessionUnbindOutcome {
    /// Removed from the tool's session set.
    Unbound,
    /// Session id was not in the tool's session set; no-op.
    NotBound,
    /// No tool with the given id is registered against this connection.
    UnknownTool,
}

/// Aggregated summary of a connection-scoped cleanup pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectionCleanupReport {
    /// Number of distinct `(connection, tool_id)` records dropped.
    pub tools_dropped: usize,
    /// Number of reverse-index `(session_id, tool_id)` rows cleaned up
    /// across every session the dropped tools were bound to.
    pub session_bindings_cleared: usize,
}

/// Aggregated summary of a session-scoped cleanup pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCleanupReport {
    /// Number of tools whose session set lost the unregistered session id.
    pub tools_touched: usize,
    /// Number of tools whose session set became empty after the
    /// unregistration. The tool record itself is NOT removed — the owning
    /// connection still owns it and may rebind via
    /// [`ToolRegistry::bind_tool_session`] later.
    pub tools_left_orphaned: usize,
}

/// Backend-agnostic registry of tools available within a router.
///
/// Methods are split into mutating (`async fn` — registration changes may
/// touch shared state and require coordination) and read-only views
/// (synchronous — implementations should answer from a consistent snapshot
/// without awaiting). The split mirrors how callers use the registry: the
/// hot path is the `find_tool` / `list_tools` view; mutations happen on the
/// rarer registration boundary.
#[async_trait]
pub trait ToolRegistry: Send + Sync + std::fmt::Debug {
    /// Register a single tool against `connection_id`.
    ///
    /// The outcome reports whether the registration created a new entry,
    /// updated an existing one, was shadowed by a higher-priority
    /// registration, or was rejected. `reg.sessions` may be empty — the
    /// tool is registered but unreachable until
    /// [`Self::bind_tool_session`] adds at least one session binding.
    /// Implementations must enforce per-`(connection_id, tool_id)`
    /// uniqueness within their plane.
    async fn register_tool(
        &self,
        connection_id: ConnectionId,
        reg: ToolRegistration,
    ) -> RegistrationOutcome;

    /// Register a multi-tool batch from a single tool server against
    /// `connection_id`.
    ///
    /// Returns one [`RegistrationOutcome`] per tool in input order. Batch
    /// semantics are best-effort: per-tool failures do not abort the rest
    /// of the batch. The whole batch shares `reg.sessions` (which may be
    /// empty).
    async fn register_server(
        &self,
        connection_id: ConnectionId,
        reg: ToolServerRegistration,
    ) -> Vec<RegistrationOutcome>;

    /// Drop the tool registered under `(connection_id, tool_id)`. Returns
    /// `true` if a matching entry was removed, `false` if no such entry
    /// existed. The tool is removed from every session it was bound to in
    /// one shot — use [`Self::unbind_tool_session`] for per-session removal.
    async fn unregister_tool(&self, connection_id: &ConnectionId, tool: &ToolId) -> bool;

    /// Drop every tool registered by `connection_id` under `server_id`.
    /// Returns the number of entries removed.
    async fn unregister_server(&self, connection_id: &ConnectionId, server: &ServerId) -> usize;

    /// Add `session_id` to the per-tool session set of
    /// `(connection_id, tool_id)`. The caller (typically the WebSocket
    /// router) is responsible for verifying that `session_id` is in the
    /// connection's bound-session set before calling this method.
    async fn bind_tool_session(
        &self,
        connection_id: &ConnectionId,
        tool: &ToolId,
        session_id: &SessionId,
    ) -> ToolSessionBindOutcome;

    /// Remove `session_id` from the per-tool session set of
    /// `(connection_id, tool_id)`. Does not unregister the tool itself.
    async fn unbind_tool_session(
        &self,
        connection_id: &ConnectionId,
        tool: &ToolId,
        session_id: &SessionId,
    ) -> ToolSessionUnbindOutcome;

    /// Drop every tool registered by `connection_id`. Used by the WebSocket
    /// transport on disconnect cleanup. Returns counters describing how
    /// much state was released.
    async fn drop_connection(&self, connection_id: &ConnectionId) -> ConnectionCleanupReport;

    /// Look up the active resolution for `(session, tool)`.
    ///
    /// Returns `None` when no entry exists or when an entry exists but is
    /// shadowed. A shadowed entry is never returned — the caller sees only
    /// the active resolution.
    fn find_tool(&self, session: &SessionId, tool: &ToolId) -> Option<ResolvedTool>;

    /// Enumerate every active tool description for `session`, filtered by
    /// the requested presentation `mode`. Implementations decide how to
    /// honour the mode (e.g. omit non-meta tools when `Concise` is set).
    fn list_tools(&self, session: &SessionId, mode: &ToolDefinitionMode) -> Vec<ToolDescription>;

    /// Enumerate active server summaries for `session`. Useful for
    /// rendering connected-integrations system reminders.
    fn list_servers(&self, session: &SessionId) -> Vec<ServerSummary>;

    /// Run a search query against the registry's index for `session`.
    /// `limit` caps the result count; the snapshot reports how many
    /// matches were hidden by the cap.
    fn search(&self, session: &SessionId, query: &str, limit: usize) -> SearchSnapshot;

    /// Drop the binding to `session` from every tool that has it. The
    /// affected tool records are NOT removed — their owning connection
    /// retains them and may rebind via [`Self::bind_tool_session`]. Called
    /// by the WebSocket transport when a session ends globally (no peer
    /// connection still holds the binding) and by the connection actor
    /// during per-disconnect cleanup.
    async fn unregister_session(&self, session: &SessionId) -> SessionCleanupReport;

    /// Helper: set of session ids currently bound to `(connection_id, tool_id)`.
    /// Returns an empty set when the tool is not registered. Mainly used
    /// by tests to assert per-tool session set invariants without leaning
    /// on the reverse index.
    fn tool_sessions(&self, connection_id: &ConnectionId, tool: &ToolId) -> HashSet<SessionId>;

    /// All servers registered by this user across all connections.
    fn list_servers_for_user(&self, user_id: &UserId) -> Vec<ServerRecord>;

    /// Look up a server by its connection ID.
    fn get_server_record(&self, connection_id: &ConnectionId) -> Option<ServerRecord>;

    /// Look up only a server's id by its connection ID. Lighter than
    /// [`Self::get_server_record`] for callers that need nothing else:
    /// implementations should override the default to avoid deep-cloning
    /// the whole record (notably its `metadata` JSON).
    fn get_server_id(&self, connection_id: &ConnectionId) -> Option<ServerId> {
        self.get_server_record(connection_id)
            .map(|record| record.server_id)
    }
}

/// Server identity captured at `register_server` time.
#[derive(Debug, Clone)]
pub struct ServerRecord {
    pub connection_id: ConnectionId,
    pub user_id: UserId,
    pub server_id: ServerId,
    pub description: String,
    pub metadata: serde_json::Value,
    pub registered_at: chrono::DateTime<chrono::Utc>,
    /// Monotonic registration stamp ([`next_registration_seq`]) — the
    /// stale-vs-revived discriminator for newest-wins (`registered_at` is display-only).
    pub registration_seq: u64,
}

/// Process-global hybrid logical clock: per-process strictly-increasing (no ties,
/// immune to NTP step-back) and epoch-seeded so stamps also roughly order across
/// replicas — only while inter-replica clock skew stays within the revive window
/// (`tool_route_ttl_ms`); past that, TTL eviction, not seq order, is the backstop.
/// The recency key for bind newest-wins and strictly-older eviction.
static REGISTRATION_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Bits reserved below the wall-clock milliseconds in a registration seq:
/// the per-process HLC bump space. Single source of truth for the layout;
/// encode/decode via [`seq_from_wall_ms`] / [`seq_wall_ms`].
pub const REGISTRATION_SEQ_SHIFT: u32 = 10;

/// Encode a wall-clock millisecond reading as a registration seq (before
/// the HLC bump applied by [`next_registration_seq`]).
pub fn seq_from_wall_ms(wall_ms: u64) -> u64 {
    wall_ms << REGISTRATION_SEQ_SHIFT
}

/// Decode the wall-clock milliseconds a registration seq was issued at
/// (inverse of [`seq_from_wall_ms`], dropping the HLC bump bits).
pub fn seq_wall_ms(seq: u64) -> u64 {
    seq >> REGISTRATION_SEQ_SHIFT
}

/// Issue the next monotonic registration stamp. See [`REGISTRATION_CLOCK`].
pub fn next_registration_seq() -> u64 {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let candidate = seq_from_wall_ms(now_ms);
    let mut prev = REGISTRATION_CLOCK.load(Ordering::Relaxed);
    loop {
        let next = candidate.max(prev + 1);
        match REGISTRATION_CLOCK.compare_exchange_weak(
            prev,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(actual) => prev = actual,
        }
    }
}

#[cfg(test)]
mod seq_tests {
    use super::next_registration_seq;

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    #[test]
    fn next_registration_seq_is_monotonic_and_epoch_seeded_under_burst() {
        let before_ms = now_ms();
        let first = next_registration_seq();
        let mut prev = first;
        const N: u64 = 50_000;
        for _ in 0..N {
            let s = next_registration_seq();
            assert!(s > prev, "must be strictly increasing: {prev} -> {s}");
            prev = s;
        }
        let after_ms = now_ms();

        assert!(
            prev - first >= N,
            "burst must advance by at least one per call: {first} -> {prev}",
        );
        let high = prev >> 10;
        assert!(
            high >= before_ms,
            "high bits ({high}) must be epoch-seeded (>= {before_ms})",
        );
        assert!(
            high <= after_ms + 1_000,
            "high bits ({high}) must track wall clock (<= {after_ms} + slack)",
        );
    }
}
