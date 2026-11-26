//! Bounded stdio MCP auto-restart.
//!
//! When [`crate::session::mcp_dispatcher::run_dispatcher`] processes a
//! window containing a [`echo_agent_mcp::servers::McpClientEventKind::TransportClosed`]
//! or [`echo_agent_mcp::servers::McpClientEventKind::HandshakeFailed`] key for a
//! **stdio** MCP server, the dispatcher hands the key off to
//! [`maybe_schedule_restart`]. That function applies the guard rails listed
//! below and, if all pass, spawns a one-shot [`auto_restart_stdio`] task that
//! sleeps + respawns up to three times before parking the server as
//! `unavailable`.
//!
//! ## Backoff
//!
//! Three attempts at exactly:
//!
//! ```text
//! attempt 1 → +1s  (t=1s)
//! attempt 2 → +4s  (t=5s)
//! attempt 3 → +16s (t=21s)
//! ```
//!
//! Encoded as [`BACKOFF`]. The full window before exhaustion is 21 s.
//!
//! ## Guard rails (skip conditions)
//!
//! These are the guard rails for where auto-restart must NOT fire.
//! Same ground truth at both check sites, BUT the **check
//! order differs by design** between the two sites — see the comparison
//! table below.
//!
//! 1. **Non-restart event kind** — `maybe_schedule_restart` short-circuits
//!    for anything other than `TransportClosed` / `HandshakeFailed`. The
//!    auto-restart loop does not see other kinds (it's never invoked for
//!    them), so this gate appears only at schedule time.
//! 2. **HTTP / HttpAuth** — auto-restart is **stdio-only**. HTTP/OAuth
//!    transports go through `reset_transport` on the next tool call,
//!    which is the existing and correct recovery path. The single
//!    [`RestartActions::is_stdio_server_configured`] question returns
//!    `false` for any non-stdio configured entry, so the gate doubles as
//!    the HTTP filter (no separate `is_http` check is needed).
//! 3. **`kill_on_drop` from config diff** —
//!    [`echo_agent_mcp::servers::start_mcp_server`] sets
//!    `kill_on_drop(true)` on the spawned `tokio::process::Command`
//!    in the `acp::McpServer::Stdio` arm. When
//!    `McpState::update_configs_diff` drops the `Arc<McpClient>` the
//!    child is SIGKILLed and the liveness watcher eventually emits
//!    `TransportClosed`. The dispatcher's
//!    [`crate::session::mcp_dispatcher::ShutdownState`] (set on
//!    `ConfigRemoved` events) is the explicit "this teardown was
//!    intentional" channel. We consult it via
//!    [`RestartActions::is_in_shutting_down`] at both check sites.
//! 4. **Disabled / not currently configured** — `update_configs_diff` or
//!    `ToggleMcpServer enabled=false` removes the stdio entry. We consult
//!    [`RestartActions::is_stdio_server_configured`] (which already
//!    folds the disabled-list check); on `false` mid-loop we emit one
//!    final [`crate::session::mcp_dispatcher::McpServerStatusReason::Disabled`]
//!    push and stop.
//! 5. **Already-Empty** — see the [`echo_agent_mcp::servers::ClientStateKind::Empty`]
//!    doc: a previous handshake exhausted attempts. Recovery from
//!    `Empty` is via the explicit `Refresh` button, not auto-restart.
//!    Enforced upstream: the liveness watcher emits `TransportClosed`
//!    only from `Ready` / `Initializing`, never from `Empty`.
//!
//! ### Check-order difference
//!
//! | Site                       | First check                        | Then                              |
//! |----------------------------|------------------------------------|-----------------------------------|
//! | [`maybe_schedule_restart`] | `is_in_shutting_down` (cheap, sync)| `is_stdio_server_configured` (async, may hit disk) |
//! | [`auto_restart_stdio`] loop| `is_stdio_server_configured`       | `is_in_shutting_down`             |
//!
//! At schedule time we shed the cheap sync check first so we never pay
//! the async + disk hit for an event we'll skip anyway. Inside the loop
//! the priority inverts: the "user removed it" path needs an explicit
//! wire push (`Reason::Disabled`) before we exit, so we check it first;
//! `shutting_down` exit needs no push (the upstream `ConfigRemoved`
//! flush already emitted one).
//!
//! ## Telemetry
//!
//! Emitted via `tracing::info!` with the metric name in the `target:`
//! field (`metrics.mcp.auto_restart.<counter>`), one target per metric.
//!
//! | Metric                              | Labels                                                      |
//! |-------------------------------------|-------------------------------------------------------------|
//! | `mcp.auto_restart.attempted`        | `server`, `attempt`                                         |
//! | `mcp.auto_restart.succeeded`        | `server`, `attempt ∈ {1,2,3}`                               |
//! | `mcp.auto_restart.exhausted`        | `server`                                                    |
//! | `mcp.auto_restart.skipped`          | `server`, `reason ∈ {shutting_down, not_configured, disabled}` |
//!
//! `attempted` is counted once per actual `respawn_stdio` call (after
//! the in-loop guards pass and the backoff sleep elapses), not at task
//! entry — so it stays honest if the configured-set flips mid-sleep.

use std::rc::Rc;
use std::time::Duration;

use agent_client_protocol as acp;
use async_trait::async_trait;
use echo_agent_mcp::servers::{McpClientEventKind, McpServerName};

use crate::session::mcp_dispatcher::{
    McpServerStatus, McpServerStatusPayload, McpServerStatusReason, SERVER_STATUS_METHOD,
    classify_source,
};

/// Exponential backoff for the three respawn attempts.
///
/// Wall-clock targets: `t=1s, t=5s, t=21s` (cumulative). Total worst-case
/// window before the task gives up and parks the server is 21 s.
pub(crate) const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];

/// Backoff between HTTP recovery attempts (first attempt is immediate).
/// Longer than the stdio [`BACKOFF`] because an HTTP MCP server (e.g.
/// `http-mcp-server`) usually drops on a rolling redeploy that takes minutes to bring
/// a healthy replica back; retrying across ~2.5 min lets it self-heal
/// instead of parking until the next tool call. 8 attempts total.
pub(crate) const HTTP_RECOVERY_BACKOFF: [Duration; 7] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(30),
];

/// Skip-reason label values surfaced on `mcp.auto_restart.skipped`.
///
/// `Disabled` vs `NotConfigured` both come from
/// [`RestartActions::is_stdio_server_configured`] returning `false`;
/// the split is temporal (schedule time vs inside the backoff loop) so
/// operators can tell "flipped off mid-restart" from "stale event".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// Server is in the dispatcher's `shutting_down` set
    /// ([`crate::session::mcp_dispatcher::ShutdownState`]).
    ShuttingDown,
    /// `is_stdio_server_configured` returned `false` at schedule
    /// time.
    NotConfigured,
    /// `is_stdio_server_configured` returned `false` inside the
    /// backoff loop.
    Disabled,
    /// A restart task for this server is already in flight
    /// ([`RestartActions::begin_restart`] returned `false`). A second
    /// `TransportClosed` / `HandshakeFailed` for the same server while
    /// the first respawn is still sleeping or mid-handshake is
    /// short-circuited here so we never spawn a duplicate task.
    InProgress,
}

impl SkipReason {
    fn as_label(self) -> &'static str {
        match self {
            Self::ShuttingDown => "shutting_down",
            Self::NotConfigured => "not_configured",
            Self::Disabled => "disabled",
            Self::InProgress => "in_progress",
        }
    }
}

/// Side effects that the auto-restart task needs. Abstracted as a trait so
/// unit tests can plug in a mock — the production binding lives next to
/// the dispatcher wiring in `acp_session.rs::SessionRestartActions`.
///
/// ## Threading contract
///
/// `?Send` matches the session actor's LocalSet: the production impl
/// holds `Arc<SessionActor>` (!Send) and the dispatcher's
/// `AcpAgentGatewaySender` (!Send via `acp::AgentSideConnection`).
/// Both [`maybe_schedule_restart`] and [`auto_restart_stdio`] call
/// `tokio::task::spawn_local` directly, which **panics** at runtime
/// if invoked outside a `LocalSet`. Callers MUST drive these
/// functions from a future running inside a `LocalSet` (the
/// session-actor pattern); any future `RestartActions` impl that
/// claims `Send + Sync` does NOT relax this requirement.
#[async_trait(?Send)]
pub(crate) trait RestartActions {
    /// Returns `true` iff the server still has a stdio entry in
    /// `McpState::configs` AND is enabled (not on the disabled list).
    /// Used both at schedule time and at the top of each backoff loop.
    async fn is_stdio_server_configured(&self, server: &str) -> bool;

    /// Returns `true` iff the server name is in the dispatcher's
    /// `shutting_down` set. The set is populated by `flush_window`
    /// when it observes an `McpClientEventKind::ConfigRemoved` event
    /// (see `mcp_dispatcher.rs`).
    fn is_in_shutting_down(&self, server: &str) -> bool;

    /// Re-run `start_mcp_server` for `server` against its current
    /// `McpState::configs` entry, drive the handshake to completion, arm
    /// the liveness watcher, and atomically swap the new
    /// `Arc<McpClient>` into `McpState::owned_clients`.
    ///
    /// **Stdio-only.** Callers gate on
    /// [`Self::is_stdio_server_configured`]; HTTP / HttpAuth never
    /// reach this method. Failure modes (returned as a sanitized
    /// `Err`) are:
    /// 1. No matching stdio config entry — racy concurrent removal.
    /// 2. `start_mcp_server` failed — spawn / OAuth-discovery /
    ///    transport-build error.
    /// 3. `ensure_initialized` failed — handshake error.
    /// 4. Post-handshake re-check of the configured
    ///    set found the server disabled/removed during the (multi-
    ///    second) handshake window; the new `Arc<McpClient>` is
    ///    dropped on the floor, `kill_on_drop` SIGKILLs the spawned
    ///    child, and an explicit "raced with config change" error
    ///    bubbles up.
    async fn respawn_stdio(&self, server: &str) -> Result<(), String>;

    /// Push an already-built `echo.agent/mcp/server_status` payload to the
    /// pager. The production impl wraps the dispatcher's gateway
    /// sender via [`forward_status`].
    fn push_status(&self, payload: &McpServerStatusPayload);

    /// Atomically claim the single in-flight restart slot for
    /// `server`. Returns `true` if the claim succeeded (no other
    /// restart task is running for this server) and `false` if a
    /// restart task is already in flight.
    ///
    /// Paired with [`Self::end_restart`] (released via an RAII guard on
    /// every exit path). Default impl is a no-op claim so mocks keep
    /// compiling; production backs it with a `HashSet` beside
    /// `ShutdownState`.
    fn begin_restart(&self, _server: &str) -> bool {
        true
    }

    /// Release the in-flight restart claim taken by
    /// [`Self::begin_restart`]. Default impl is a no-op (pairs with the
    /// default `begin_restart`).
    fn end_restart(&self, _server: &str) {}

    /// Returns `true` iff the server still has an **HTTP / SSE** entry in
    /// `McpState::configs` AND is enabled (not on the disabled list).
    ///
    /// HTTP analog of [`Self::is_stdio_server_configured`]; gates
    /// [`maybe_schedule_http_recovery`]. Default `false` for mocks.
    async fn is_http_server_configured(&self, _server: &str) -> bool {
        false
    }

    /// Recover a dead HTTP client in place: reset transport, re-handshake,
    /// re-arm liveness. The `Arc<McpClient>` stays in `owned_clients` (tools
    /// stay valid). Status is emitted by `ensure_initialized`, not here.
    /// Default `Err` for mocks.
    async fn reset_http_client(&self, _server: &str) -> Result<(), String> {
        Err("reset_http_client not implemented".to_string())
    }

    /// Drop `server`'s tools from the bridge after stdio restart exhaustion,
    /// so the model stops calling a `not found` server. Default no-op for mocks.
    fn unregister_server_tools(&self, _server: &str) {}
}

/// Decide whether to schedule an [`auto_restart_stdio`] task for the
/// given event, applying the guard rails (see the module doc and the
/// inline `Guard N` comments below). Returns `true` iff a task was
/// spawned; `false` for any guard-rail rejection or non-restart kind.
///
/// Calls `tokio::task::spawn_local`, so it MUST run inside a `LocalSet`
/// — in production the dispatcher's `run_dispatcher` task is.
pub(crate) async fn maybe_schedule_restart(
    actions: Rc<dyn RestartActions>,
    session_id: String,
    server: McpServerName,
    kind: McpClientEventKind,
    cancel: tokio_util::sync::CancellationToken,
) -> bool {
    // Guard 1: only transport-dead events trigger a restart.
    if !matches!(
        kind,
        McpClientEventKind::TransportClosed | McpClientEventKind::HandshakeFailed
    ) {
        return false;
    }

    // Guard 2: kill_on_drop grace window from a config diff / toggle
    // (cheap sync check before the async configured-set probe).
    if actions.is_in_shutting_down(&server) {
        record_skipped(&server, SkipReason::ShuttingDown);
        return false;
    }

    // Guard 3: must be currently configured as stdio. HTTP/HttpAuth
    // are out of scope (their `is_stdio_server_configured` impl
    // returns false for non-stdio entries). A server removed from
    // `configs` between the event firing and us checking also lands
    // here.
    if !actions.is_stdio_server_configured(&server).await {
        record_skipped(&server, SkipReason::NotConfigured);
        return false;
    }

    // Guard 4: dedup against an already-in-flight restart. A second
    // event in a later coalesce window must NOT spawn a duplicate —
    // two tasks would each `start_mcp_server` and race on
    // `owned_clients.insert`, orphaning a stdio child. The claim is
    // atomic: no `.await` between here and the `spawn_local` below.
    // Released by the RAII guard on every exit path.
    if !actions.begin_restart(&server) {
        record_skipped(&server, SkipReason::InProgress);
        return false;
    }

    let task_actions = Rc::clone(&actions);
    tokio::task::spawn_local(async move {
        // RAII: release the in-flight claim taken above when the task
        // exits for any reason.
        let _in_flight = RestartInFlightGuard {
            actions: Rc::clone(&task_actions),
            server: server.clone(),
        };
        auto_restart_stdio(task_actions, session_id, server, cancel).await;
    });
    true
}

/// RAII guard that releases the in-flight restart claim taken by
/// [`maybe_schedule_restart`] via [`RestartActions::begin_restart`].
/// Dropped when the spawned [`auto_restart_stdio`] task exits — on
/// success, exhaustion, a guard-rail skip, cancellation, or a panic —
/// so a future `TransportClosed` for the same server can schedule a
/// fresh restart.
struct RestartInFlightGuard {
    actions: Rc<dyn RestartActions>,
    server: McpServerName,
}

impl Drop for RestartInFlightGuard {
    fn drop(&mut self) {
        self.actions.end_restart(&self.server);
    }
}

/// One-shot task: sleep, re-check guard rails, respawn, repeat (≤3
/// attempts), emitting the `mcp.auto_restart.*` metrics. Must run
/// inside a `LocalSet` (the production `RestartActions` holds `!Send`
/// types).
///
/// Each iteration re-checks the guards in the inverse order of
/// [`maybe_schedule_restart`] (see the module doc § "Check-order
/// difference"): `is_stdio_server_configured` first — a mid-backoff
/// removal emits a final `Reason::Disabled` push — then
/// `is_in_shutting_down` (no push; the `ConfigRemoved` flush already
/// emitted one).
///
/// On `Ok` it emits `Reason::RestartSucceeded`; this is the SOLE
/// success emitter, since `respawn_stdio` wires `set_event_tx` AFTER
/// `ensure_initialized` so the dispatcher's `Ready → Initialized`
/// mapping does not fire. On `Err` it emits `Reason::RestartFailed`
/// and continues; after three failures the server is parked (recovery
/// is via explicit Refresh).
pub(crate) async fn auto_restart_stdio(
    actions: Rc<dyn RestartActions>,
    session_id: String,
    server: McpServerName,
    cancel: tokio_util::sync::CancellationToken,
) {
    for (idx, wait) in BACKOFF.iter().enumerate() {
        let attempt = idx + 1;

        // On graceful shutdown the dispatcher cancels this token;
        // select on it so the backoff sleep aborts promptly instead of
        // delaying shutdown or pushing through a tearing-down gateway.
        tokio::select! {
            _ = tokio::time::sleep(*wait) => {}
            _ = cancel.cancelled() => {
                tracing::debug!(
                    server = %server,
                    attempt,
                    "auto-restart cancelled during backoff (session shutdown)",
                );
                return;
            }
        }

        // Also short-circuit before the (multi-second) respawn call if
        // cancellation landed between the sleep completing and now.
        if cancel.is_cancelled() {
            tracing::debug!(
                server = %server,
                attempt,
                "auto-restart cancelled before respawn (session shutdown)",
            );
            return;
        }

        // HTTP/HttpAuth are filtered at schedule time, so the
        // `Reason::Disabled` push below only fires for user-driven
        // removal (toggle-off / config diff).
        if !actions.is_stdio_server_configured(&server).await {
            tracing::info!(
                server = %server,
                attempt,
                "auto-restart aborted: server no longer configured",
            );
            record_skipped(&server, SkipReason::Disabled);
            push(
                &*actions,
                &session_id,
                &server,
                McpServerStatus::Unavailable,
                McpServerStatusReason::Disabled,
                None,
            );
            return;
        }
        if actions.is_in_shutting_down(&server) {
            tracing::info!(
                server = %server,
                attempt,
                "auto-restart aborted: server in shutting_down set",
            );
            record_skipped(&server, SkipReason::ShuttingDown);
            return;
        }

        record_attempted(&server, attempt);

        match actions.respawn_stdio(&server).await {
            Ok(()) => {
                tracing::info!(
                    server = %server,
                    attempt,
                    "auto-restart succeeded",
                );
                record_succeeded(&server, attempt);
                push(
                    &*actions,
                    &session_id,
                    &server,
                    McpServerStatus::Ready,
                    McpServerStatusReason::RestartSucceeded,
                    None,
                );
                return;
            }
            Err(reason) => {
                tracing::warn!(
                    server = %server,
                    attempt,
                    %reason,
                    "auto-restart attempt failed",
                );
                push(
                    &*actions,
                    &session_id,
                    &server,
                    McpServerStatus::Unavailable,
                    McpServerStatusReason::RestartFailed,
                    Some(format!(
                        "attempt {} of {}: {}",
                        attempt,
                        BACKOFF.len(),
                        reason
                    )),
                );
            }
        }
    }

    // All three attempts failed — park the server.
    record_exhausted(&server);
    push(
        &*actions,
        &session_id,
        &server,
        McpServerStatus::Unavailable,
        McpServerStatusReason::RestartFailed,
        Some(format!("exhausted after {} attempts", BACKOFF.len())),
    );
    // The evicted client was never replaced; its tools are still registered.
    // Drop them so the model stops calling a `not found` server.
    actions.unregister_server_tools(&server);
}

/// HTTP counterpart to [`maybe_schedule_restart`]: retries
/// `reset_http_client` on the [`HTTP_RECOVERY_BACKOFF`] ladder so a dropped
/// HTTP client self-heals. Pushes no status (`ensure_initialized` owns it).
/// Same guard rails as [`maybe_schedule_restart`] (shutting-down /
/// configured / in-flight dedup). Returns `true` iff a task was spawned;
/// must run inside a `LocalSet`.
pub(crate) async fn maybe_schedule_http_recovery(
    actions: Rc<dyn RestartActions>,
    server: McpServerName,
    cancel: tokio_util::sync::CancellationToken,
) -> bool {
    // Guard: intentional teardown (config diff / toggle-off).
    if actions.is_in_shutting_down(&server) {
        record_http_recovery_skipped(&server, SkipReason::ShuttingDown);
        return false;
    }

    // Guard: must still be an enabled HTTP/SSE entry.
    if !actions.is_http_server_configured(&server).await {
        record_http_recovery_skipped(&server, SkipReason::NotConfigured);
        return false;
    }

    // Guard: dedup. Shares the `in_flight_restart` slot with stdio respawn.
    // Atomic: no `.await` between the claim and `spawn_local`.
    if !actions.begin_restart(&server) {
        record_http_recovery_skipped(&server, SkipReason::InProgress);
        return false;
    }

    let task_actions = Rc::clone(&actions);
    tokio::task::spawn_local(async move {
        // RAII: release the in-flight claim on every exit path.
        let _in_flight = RestartInFlightGuard {
            actions: Rc::clone(&task_actions),
            server: server.clone(),
        };
        http_recovery_loop(task_actions, server, cancel).await;
    });
    true
}

/// Retry loop backing [`maybe_schedule_http_recovery`]: immediate attempt,
/// then back off on [`HTTP_RECOVERY_BACKOFF`], re-checking the guards each
/// time. Returns on success, a tripped guard, or cancellation; parks the
/// server (metric only) once exhausted. Emits no status pushes —
/// `ensure_initialized` owns the server's status. Must run in a `LocalSet`.
async fn http_recovery_loop(
    actions: Rc<dyn RestartActions>,
    server: McpServerName,
    cancel: tokio_util::sync::CancellationToken,
) {
    // `wait_before`: delay before each attempt — `None` for the immediate
    // first, then each `HTTP_RECOVERY_BACKOFF` step.
    let waits = std::iter::once(None).chain(HTTP_RECOVERY_BACKOFF.iter().map(Some));
    let total = HTTP_RECOVERY_BACKOFF.len() + 1;

    for (idx, wait_before) in waits.enumerate() {
        let attempt = idx + 1;

        if let Some(wait) = wait_before {
            // Abort the sleep promptly on shutdown instead of holding the claim.
            tokio::select! {
                _ = tokio::time::sleep(*wait) => {}
                _ = cancel.cancelled() => return,
            }
        }
        if cancel.is_cancelled() {
            return;
        }

        // Re-check guards each attempt: a config toggle-off / shutdown can
        // land between attempts (same LocalSet).
        if actions.is_in_shutting_down(&server) {
            record_http_recovery_skipped(&server, SkipReason::ShuttingDown);
            return;
        }
        if !actions.is_http_server_configured(&server).await {
            record_http_recovery_skipped(&server, SkipReason::Disabled);
            return;
        }

        record_http_recovery_attempted(&server);
        match actions.reset_http_client(&server).await {
            Ok(()) => {
                tracing::info!(
                    server = %server,
                    attempt,
                    "in-place HTTP transport recovery succeeded",
                );
                record_http_recovery_succeeded(&server);
                return;
            }
            Err(reason) => {
                // Keep retrying; the `Pending` client keeps lazy recovery alive.
                tracing::warn!(
                    server = %server,
                    attempt,
                    %reason,
                    "in-place HTTP transport recovery attempt failed",
                );
            }
        }
    }

    // Ladder exhausted — park the server; a later tool call still triggers
    // lazy recovery via `ensure_initialized`.
    record_http_recovery_exhausted(&server);
    tracing::warn!(
        server = %server,
        attempts = total,
        "in-place HTTP transport recovery exhausted; server parked until next tool call",
    );
}

/// Build a wire payload and hand it to the actions' `push_status` hook.
fn push(
    actions: &dyn RestartActions,
    session_id: &str,
    server: &str,
    status: McpServerStatus,
    reason: McpServerStatusReason,
    detail: Option<String>,
) {
    let payload = McpServerStatusPayload {
        session_id: session_id.to_string(),
        name: server.to_string(),
        source: classify_source(server),
        status,
        reason,
        detail,
        tools: None,
    };
    actions.push_status(&payload);
}

/// Serialize a [`McpServerStatusPayload`] and send it to the gateway as an
/// ACP `echo.agent/mcp/server_status` notification. Failures are logged and
/// dropped — restart-task pushes must not block the session actor.
///
/// Public so production impls and tests can wrap a gateway sender
/// without reaching into private dispatcher internals. Uses
/// [`crate::session::mcp_dispatcher::SERVER_STATUS_METHOD`] so pushes
/// share the dispatcher's wire method name.
pub(crate) fn forward_status(
    gateway: &echo_agent_acp::AcpAgentGatewaySender,
    payload: &McpServerStatusPayload,
) {
    let raw = match serde_json::value::to_raw_value(payload) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                server = %payload.name,
                error = %e,
                "auto-restart: failed to serialize mcp/server_status payload",
            );
            return;
        }
    };
    gateway.forward_fire_and_forget(acp::ExtNotification::new(SERVER_STATUS_METHOD, raw.into()));
}

// ── telemetry helpers (tracing-as-metrics; see module doc § Telemetry) ──

fn record_attempted(server: &str, attempt: usize) {
    tracing::info!(
        target: "metrics.mcp.auto_restart.attempted",
        server = %server,
        attempt,
    );
}
fn record_succeeded(server: &str, attempt: usize) {
    tracing::info!(target: "metrics.mcp.auto_restart.succeeded", server = %server, attempt);
}
fn record_exhausted(server: &str) {
    tracing::info!(target: "metrics.mcp.auto_restart.exhausted", server = %server);
}
fn record_skipped(server: &str, reason: SkipReason) {
    tracing::info!(
        target: "metrics.mcp.auto_restart.skipped",
        server = %server,
        reason = reason.as_label(),
    );
}

// ── in-place HTTP recovery metrics (kept separate from auto_restart.* so
//    operators can distinguish stdio respawn from HTTP transport reset) ──

fn record_http_recovery_attempted(server: &str) {
    tracing::info!(target: "metrics.mcp.http_recovery.attempted", server = %server);
}
fn record_http_recovery_succeeded(server: &str) {
    tracing::info!(target: "metrics.mcp.http_recovery.succeeded", server = %server);
}
fn record_http_recovery_exhausted(server: &str) {
    tracing::info!(target: "metrics.mcp.http_recovery.exhausted", server = %server);
}
fn record_http_recovery_skipped(server: &str, reason: SkipReason) {
    tracing::info!(
        target: "metrics.mcp.http_recovery.skipped",
        server = %server,
        reason = reason.as_label(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::time::Duration as StdDuration;

    /// Records `RestartActions` calls for assertion. All fields are
    /// `RefCell`-wrapped because the production trait takes `&self`
    /// and the auto-restart task threads a single `Rc<dyn ...>`
    /// through the loop. The production trait is `Rc<dyn RestartActions>`,
    /// so tests share the same `Rc` directly.
    #[derive(Default)]
    struct MockActions {
        configured: RefCell<HashSet<String>>,
        shutting_down: RefCell<HashSet<String>>,
        /// Scripted respawn outcomes. `pop_front` per attempt; if the
        /// deque empties before the loop completes, attempts past the
        /// scripted ones return `Err("not scripted")` (which surfaces a
        /// test bug rather than silently passing).
        respawn_outcomes: RefCell<std::collections::VecDeque<Result<(), String>>>,
        respawn_calls: RefCell<Vec<String>>,
        pushes: RefCell<Vec<McpServerStatusPayload>>,
        /// Servers with an in-flight restart claim (mirrors the
        /// production `ShutdownState::in_flight_restart` set) so the
        /// dedup guard in `maybe_schedule_restart` can be exercised.
        in_flight: RefCell<HashSet<String>>,
        /// Servers configured as HTTP/SSE (for `is_http_server_configured`).
        http_configured: RefCell<HashSet<String>>,
        /// Scripted `reset_http_client` outcomes, per server.
        reset_outcomes: RefCell<
            std::collections::HashMap<String, std::collections::VecDeque<Result<(), String>>>,
        >,
        /// Recorded `reset_http_client` calls.
        reset_calls: RefCell<Vec<String>>,
        /// Recorded `unregister_server_tools` calls.
        unregister_calls: RefCell<Vec<String>>,
    }

    impl MockActions {
        fn new() -> Self {
            Self::default()
        }
        fn configure(&self, name: &str) {
            self.configured.borrow_mut().insert(name.to_string());
        }
        fn unconfigure(&self, name: &str) {
            self.configured.borrow_mut().remove(name);
        }
        fn mark_shutting_down(&self, name: &str) {
            self.shutting_down.borrow_mut().insert(name.to_string());
        }
        fn script_outcome(&self, outcome: Result<(), String>) {
            self.respawn_outcomes.borrow_mut().push_back(outcome);
        }
        fn respawn_call_count(&self) -> usize {
            self.respawn_calls.borrow().len()
        }
        fn pushes(&self) -> Vec<McpServerStatusPayload> {
            self.pushes.borrow().clone()
        }
        fn configure_http(&self, name: &str) {
            self.http_configured.borrow_mut().insert(name.to_string());
        }
        fn script_reset(&self, name: &str, outcome: Result<(), String>) {
            self.reset_outcomes
                .borrow_mut()
                .entry(name.to_string())
                .or_default()
                .push_back(outcome);
        }
        fn reset_calls(&self) -> Vec<String> {
            self.reset_calls.borrow().clone()
        }
        fn unregister_calls(&self) -> Vec<String> {
            self.unregister_calls.borrow().clone()
        }
    }

    #[async_trait(?Send)]
    impl RestartActions for MockActions {
        async fn is_stdio_server_configured(&self, server: &str) -> bool {
            self.configured.borrow().contains(server)
        }
        fn is_in_shutting_down(&self, server: &str) -> bool {
            self.shutting_down.borrow().contains(server)
        }
        async fn respawn_stdio(&self, server: &str) -> Result<(), String> {
            self.respawn_calls.borrow_mut().push(server.to_string());
            self.respawn_outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err("not scripted".to_string()))
        }
        fn push_status(&self, payload: &McpServerStatusPayload) {
            self.pushes.borrow_mut().push(payload.clone());
        }
        fn begin_restart(&self, server: &str) -> bool {
            self.in_flight.borrow_mut().insert(server.to_string())
        }
        fn end_restart(&self, server: &str) {
            self.in_flight.borrow_mut().remove(server);
        }
        async fn is_http_server_configured(&self, server: &str) -> bool {
            self.http_configured.borrow().contains(server)
        }
        async fn reset_http_client(&self, server: &str) -> Result<(), String> {
            self.reset_calls.borrow_mut().push(server.to_string());
            self.reset_outcomes
                .borrow_mut()
                .get_mut(server)
                .and_then(|q| q.pop_front())
                .unwrap_or_else(|| Err("not scripted".to_string()))
        }
        fn unregister_server_tools(&self, server: &str) {
            self.unregister_calls.borrow_mut().push(server.to_string());
        }
    }

    fn dyn_actions(mock: Rc<MockActions>) -> Rc<dyn RestartActions> {
        mock
    }

    /// A never-cancelled token for the happy-path tests.
    fn never_cancel() -> tokio_util::sync::CancellationToken {
        tokio_util::sync::CancellationToken::new()
    }

    async fn run_in_local<F, T>(f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let local = tokio::task::LocalSet::new();
        local.run_until(f).await
    }

    /// Contract: with all 3 attempts failing, respawn is called at
    /// `t=1s`, `t=5s`, `t=21s`. Uses `tokio::time::pause` +
    /// `advance(21s)`.
    #[tokio::test(start_paused = true)]
    async fn backoff_attempts_sequence() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Err("e1".into()));
            mock.script_outcome(Err("e2".into()));
            mock.script_outcome(Err("e3".into()));

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            // t=0: nothing yet
            tokio::task::yield_now().await;
            assert_eq!(mock.respawn_call_count(), 0);

            // t=1s: first attempt fires
            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(mock.respawn_call_count(), 1);

            // t=5s: second attempt fires (after 4s wait)
            tokio::time::advance(StdDuration::from_secs(4)).await;
            tokio::task::yield_now().await;
            assert_eq!(mock.respawn_call_count(), 2);

            // t=21s: third attempt fires (after 16s wait)
            tokio::time::advance(StdDuration::from_secs(16)).await;
            tokio::task::yield_now().await;
            assert_eq!(mock.respawn_call_count(), 3);

            task.await.unwrap();
        })
        .await;
    }

    /// Contract: if the server is removed from configs between the
    /// schedule call and the first backoff fires, respawn is NOT
    /// called and `mcp.auto_restart.skipped{reason="not_configured"}`
    /// is emitted (via `Reason::Disabled` push on the wire — see
    /// auto_restart_stdio rustdoc).
    #[tokio::test(start_paused = true)]
    async fn skip_when_not_configured() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            // Remove the config BEFORE the first 1s sleep elapses.
            mock.unconfigure("svr");
            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(
                mock.respawn_call_count(),
                0,
                "respawn must not run for an unconfigured server",
            );
            // The on-the-wire push is `Reason::Disabled` (not
            // `RestartFailed`) — see auto_restart_stdio rustdoc.
            let pushes = mock.pushes();
            assert_eq!(pushes.len(), 1);
            assert_eq!(pushes[0].reason, McpServerStatusReason::Disabled);
            assert_eq!(pushes[0].status, McpServerStatus::Unavailable);
        })
        .await;
    }

    /// Contract: same shape as `skip_when_not_configured` but the
    /// trigger is a toggle-disable (modeled the same way by
    /// `MockActions::unconfigure`). Verifies that the disabled-by-toggle
    /// path produces the same `Reason::Disabled` push that the
    /// not-configured path does — the wire schema is intentionally
    /// uniform here so the pager can render either with the same
    /// "disabled" affordance.
    #[tokio::test(start_paused = true)]
    async fn skip_when_disabled() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            // ToggleMcpServer(enabled=false) effectively drops the
            // entry from configs in the same way as a config-diff
            // removal.
            mock.unconfigure("svr");
            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(mock.respawn_call_count(), 0);
            let pushes = mock.pushes();
            assert_eq!(pushes.len(), 1);
            assert_eq!(pushes[0].reason, McpServerStatusReason::Disabled);
        })
        .await;
    }

    /// Contract: `maybe_schedule_restart` returns `false` (no task
    /// spawned) when the server is already in the dispatcher's
    /// `shutting_down` set. The kill_on_drop guard rail.
    #[tokio::test(start_paused = true)]
    async fn skip_when_in_shutting_down_set() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.mark_shutting_down("svr");

            let spawned = maybe_schedule_restart(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                McpClientEventKind::TransportClosed,
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(!spawned);
            assert_eq!(mock.respawn_call_count(), 0);
            assert!(mock.pushes().is_empty());
        })
        .await;
    }

    /// Contract: HTTP-only servers never schedule a restart. We
    /// simulate the "not stdio" case by leaving the server
    /// **unconfigured** — production `is_stdio_server_configured`
    /// already returns `false` for HTTP/HttpAuth entries (see
    /// `acp_session.rs` impl). The dispatcher's TransportClosed event
    /// reaches `maybe_schedule_restart`, fails the stdio gate, emits
    /// `mcp.auto_restart.skipped{reason="not_configured"}`, and does
    /// NOT spawn the task.
    #[tokio::test(start_paused = true)]
    async fn http_event_does_not_trigger_restart() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            // Intentionally not configured as stdio — mirrors
            // production's gate behavior for HTTP servers.

            let spawned = maybe_schedule_restart(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "http-only".to_string(),
                McpClientEventKind::TransportClosed,
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(
                !spawned,
                "HTTP/HttpAuth events must not spawn restart tasks"
            );
            assert_eq!(mock.respawn_call_count(), 0);
            assert!(mock.pushes().is_empty());
        })
        .await;
    }

    /// Contract (in-flight dedup): if a restart task
    /// is already in flight for a server, a second
    /// `maybe_schedule_restart` for the same server returns `false`,
    /// does NOT spawn a duplicate task, and emits
    /// `mcp.auto_restart.skipped{reason="in_progress"}`. Modeled by
    /// pre-claiming the in-flight slot (which the production
    /// `ShutdownState` set does atomically).
    #[tokio::test(start_paused = true)]
    async fn dedup_skips_when_restart_already_in_flight() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            // Simulate an already-running restart task by claiming the
            // in-flight slot up front.
            assert!(mock.begin_restart("svr"));

            let spawned = maybe_schedule_restart(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                McpClientEventKind::TransportClosed,
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(!spawned, "must not spawn a duplicate restart task");
            assert_eq!(mock.respawn_call_count(), 0);
            assert!(mock.pushes().is_empty());
        })
        .await;
    }

    /// Contract (cancellation): cancelling the token
    /// before the first backoff sleep elapses aborts the task without
    /// calling `respawn_stdio` or emitting any wire push.
    #[tokio::test(start_paused = true)]
    async fn cancellation_aborts_backoff_before_respawn() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Ok(()));
            let cancel = tokio_util::sync::CancellationToken::new();

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                cancel.clone(),
            ));

            // Cancel during the first 1s backoff sleep.
            cancel.cancel();
            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(
                mock.respawn_call_count(),
                0,
                "cancelled task must not respawn",
            );
            assert!(
                mock.pushes().is_empty(),
                "cancelled task must not push status",
            );
        })
        .await;
    }

    /// Contract: a successful respawn pushes EXACTLY ONE wire
    /// notification, with `Reason::RestartSucceeded` (NOT
    /// `Initialized` — that's reserved for the first-time
    /// `ensure_initialized` Ready emit — AND NOT duplicated by a
    /// dispatcher-emitted `Initialized`).
    #[tokio::test(start_paused = true)]
    async fn respawn_emits_ready_with_reason_restart_succeeded() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Ok(()));

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(mock.respawn_call_count(), 1);
            let pushes = mock.pushes();
            // Exactly one push per success. Production's respawn_stdio
            // wires set_event_tx AFTER ensure_initialized, so the
            // dispatcher's Ready-mapping does not also emit an
            // Initialized push (which would make two).
            assert_eq!(
                pushes.len(),
                1,
                "exactly one push per successful restart; got {pushes:?}"
            );
            assert_eq!(pushes[0].reason, McpServerStatusReason::RestartSucceeded);
            assert_ne!(pushes[0].reason, McpServerStatusReason::Initialized);
            assert_eq!(pushes[0].status, McpServerStatus::Ready);
        })
        .await;
    }

    /// Contract: three failed attempts produce three intermediate
    /// `Reason::RestartFailed` pushes (attempt 1, 2, 3) plus one final
    /// `Reason::RestartFailed` carrying `detail="exhausted after 3
    /// attempts"`.
    ///
    /// ## Telemetry coverage caveat
    ///
    /// The `mcp.auto_restart.exhausted` and per-attempt
    /// `mcp.auto_restart.attempted` counters are emitted via
    /// `tracing::info!` with metric-name `target:`s. This test does
    /// NOT install a `tracing` subscriber — if a future refactor
    /// accidentally deletes the `record_exhausted` / `record_attempted`
    /// calls, the wire-push assertion below would still pass while
    /// the counters silently disappear from telemetry. Acceptable because
    /// both call sites are right next to the wire push and likely to
    /// be deleted/edited together; tighter coverage is a follow-up.
    #[tokio::test(start_paused = true)]
    async fn all_three_attempts_fail_emits_exhausted_telemetry() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Err("transport reset".into()));
            mock.script_outcome(Err("spawn failed".into()));
            mock.script_outcome(Err("handshake timeout".into()));

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            tokio::time::advance(StdDuration::from_secs(21)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(mock.respawn_call_count(), 3);
            let pushes = mock.pushes();
            // 3 per-attempt RestartFailed + 1 final exhausted RestartFailed.
            assert_eq!(pushes.len(), 4, "got pushes: {pushes:?}");
            for p in &pushes {
                assert_eq!(p.reason, McpServerStatusReason::RestartFailed);
                assert_eq!(p.status, McpServerStatus::Unavailable);
            }
            // Per-attempt details encode their attempt index.
            assert!(
                pushes[0]
                    .detail
                    .as_deref()
                    .map(|s| s.starts_with("attempt 1 of 3"))
                    .unwrap_or(false),
                "first push detail: {:?}",
                pushes[0].detail,
            );
            assert!(
                pushes[2]
                    .detail
                    .as_deref()
                    .map(|s| s.starts_with("attempt 3 of 3"))
                    .unwrap_or(false),
                "third push detail: {:?}",
                pushes[2].detail,
            );
            // Final push carries the exhausted marker.
            assert_eq!(
                pushes[3].detail.as_deref(),
                Some("exhausted after 3 attempts"),
            );
        })
        .await;
    }

    /// Contract: exhausting all three stdio respawn attempts unregisters
    /// the dead server's tools (so the model stops dispatching against a
    /// `not found` server) AND emits the four `RestartFailed` pushes.
    #[tokio::test(start_paused = true)]
    async fn exhaustion_unregisters_server_tools() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Err("e1".into()));
            mock.script_outcome(Err("e2".into()));
            mock.script_outcome(Err("e3".into()));

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            tokio::time::advance(StdDuration::from_secs(21)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert_eq!(mock.respawn_call_count(), 3);
            assert_eq!(
                mock.unregister_calls(),
                vec!["svr".to_string()],
                "exhausted restart must unregister the dead server's tools exactly once",
            );
        })
        .await;
    }

    /// Contract: a successful stdio respawn does NOT unregister tools —
    /// the recovered client serves the same registered tools.
    #[tokio::test(start_paused = true)]
    async fn successful_restart_keeps_tools_registered() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure("svr");
            mock.script_outcome(Ok(()));

            let task = tokio::task::spawn_local(auto_restart_stdio(
                dyn_actions(mock.clone()),
                "sess-1".to_string(),
                "svr".to_string(),
                never_cancel(),
            ));

            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            task.await.unwrap();

            assert!(
                mock.unregister_calls().is_empty(),
                "a recovered server must keep its tools registered",
            );
        })
        .await;
    }

    /// Contract: a `TransportClosed` for a configured HTTP server
    /// schedules an in-place `reset_http_client` (NOT a respawn) and emits
    /// no status of its own (`ensure_initialized` owns that).
    #[tokio::test(start_paused = true)]
    async fn http_recovery_schedules_reset_in_place() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            mock.script_reset("http-mcp-server", Ok(()));

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            assert!(spawned);
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            assert_eq!(mock.reset_calls(), vec!["http-mcp-server".to_string()]);
            assert_eq!(
                mock.respawn_call_count(),
                0,
                "HTTP recovery must not spawn a stdio respawn",
            );
            assert!(
                mock.pushes().is_empty(),
                "HTTP recovery relies on ensure_initialized for status; no direct push",
            );
        })
        .await;
    }

    /// Contract: HTTP recovery is skipped for a server that is not a
    /// configured/enabled HTTP entry (e.g. removed, disabled, or stdio).
    #[tokio::test(start_paused = true)]
    async fn http_recovery_skips_when_not_http_configured() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            // Intentionally not http-configured.
            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(!spawned);
            assert!(mock.reset_calls().is_empty());
        })
        .await;
    }

    /// Contract: HTTP recovery respects the `shutting_down` guard (config
    /// diff / toggle-off) — no reset is scheduled.
    #[tokio::test(start_paused = true)]
    async fn http_recovery_skips_when_shutting_down() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            mock.mark_shutting_down("http-mcp-server");

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(!spawned);
            assert!(mock.reset_calls().is_empty());
        })
        .await;
    }

    /// Contract: if the server is marked `shutting_down` AFTER scheduling
    /// but BEFORE the spawned task runs, the in-task re-check bails — no
    /// `reset_http_client`.
    #[tokio::test(start_paused = true)]
    async fn http_recovery_rechecks_shutting_down_before_reset() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            mock.script_reset("http-mcp-server", Ok(()));

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            assert!(spawned);
            // Teardown lands before the spawned task gets to run.
            mock.mark_shutting_down("http-mcp-server");
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            assert!(
                mock.reset_calls().is_empty(),
                "task must re-check shutting_down and skip the reset",
            );
        })
        .await;
    }

    /// Contract: if the server is unconfigured/disabled AFTER scheduling but
    /// BEFORE the spawned task runs, the in-task re-check bails — no
    /// `reset_http_client`.
    #[tokio::test(start_paused = true)]
    async fn http_recovery_rechecks_configured_before_reset() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            mock.script_reset("http-mcp-server", Ok(()));

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            assert!(spawned);
            // Server removed/disabled before the task runs.
            mock.http_configured.borrow_mut().remove("http-mcp-server");
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;

            assert!(
                mock.reset_calls().is_empty(),
                "task must re-check configured and skip the reset",
            );
        })
        .await;
    }

    /// Contract: HTTP recovery dedups against an in-flight recovery/restart
    /// for the same server (shared `begin_restart` slot).
    #[tokio::test(start_paused = true)]
    async fn http_recovery_dedups_when_already_in_flight() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            assert!(mock.begin_restart("http-mcp-server"));

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            tokio::task::yield_now().await;

            assert!(!spawned, "must not schedule a duplicate recovery");
            assert!(mock.reset_calls().is_empty());
        })
        .await;
    }

    /// Contract: a failed first `reset_http_client` is retried on the
    /// [`HTTP_RECOVERY_BACKOFF`] ladder rather than parking after one shot.
    /// First attempt is immediate (`t=0`), the retry fires after the first
    /// backoff step (`t=1s`), and once it succeeds the loop stops.
    #[tokio::test(start_paused = true)]
    async fn http_recovery_retries_on_backoff_until_success() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            // First attempt fails (e.g. backend mid-redeploy), second wins.
            mock.script_reset("http-mcp-server", Err("transport closed".into()));
            mock.script_reset("http-mcp-server", Ok(()));

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            assert!(spawned);

            // t=0: immediate first attempt fails; no retry yet.
            tokio::task::yield_now().await;
            assert_eq!(mock.reset_calls().len(), 1, "first attempt is immediate");

            // t=1s: first backoff step elapses → second attempt succeeds.
            tokio::time::advance(StdDuration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                mock.reset_calls(),
                vec!["http-mcp-server".to_string(), "http-mcp-server".to_string()],
                "failed attempt must be retried after the first backoff step",
            );

            // No further attempts after success.
            tokio::time::advance(StdDuration::from_secs(60)).await;
            tokio::task::yield_now().await;
            assert_eq!(mock.reset_calls().len(), 2, "loop stops once recovered");
            assert!(
                mock.pushes().is_empty(),
                "HTTP recovery relies on ensure_initialized for status; no direct push",
            );
        })
        .await;
    }

    /// Contract: when every attempt fails, the loop tries once per
    /// `HTTP_RECOVERY_BACKOFF` step plus the immediate attempt, then parks
    /// the server (no more resets, no status push).
    #[tokio::test(start_paused = true)]
    async fn http_recovery_parks_after_exhausting_backoff() {
        run_in_local(async {
            let mock = Rc::new(MockActions::new());
            mock.configure_http("http-mcp-server");
            // Script one more failure than the total attempts so an
            // unexpected extra attempt would still be a scripted Err (and
            // the count assertion below catches it).
            for _ in 0..HTTP_RECOVERY_BACKOFF.len() + 2 {
                mock.script_reset("http-mcp-server", Err("still down".into()));
            }

            let spawned = maybe_schedule_http_recovery(
                dyn_actions(mock.clone()),
                "http-mcp-server".to_string(),
                never_cancel(),
            )
            .await;
            assert!(spawned);

            // Drive the whole ladder: immediate attempt + every backoff step.
            tokio::task::yield_now().await;
            for wait in HTTP_RECOVERY_BACKOFF {
                tokio::time::advance(wait).await;
                tokio::task::yield_now().await;
            }
            // Allow the parked/exhaustion path to run.
            tokio::time::advance(StdDuration::from_secs(60)).await;
            tokio::task::yield_now().await;

            assert_eq!(
                mock.reset_calls().len(),
                HTTP_RECOVERY_BACKOFF.len() + 1,
                "one immediate attempt plus one per backoff step, then park",
            );
            assert!(
                mock.pushes().is_empty(),
                "exhaustion parks silently; ensure_initialized owns status",
            );
        })
        .await;
    }

    /// `forward_status` and the dispatcher must agree on the wire
    /// method name. If someone renames
    /// `SERVER_STATUS_METHOD` only one path follows — this pinning
    /// test breaks loudly. We don't probe an actual ACP gateway —
    /// just assert the const referenced by `forward_status` is the
    /// same one re-exported by `mcp_dispatcher`.
    #[test]
    fn forward_status_uses_dispatcher_method() {
        assert_eq!(
            crate::session::mcp_dispatcher::SERVER_STATUS_METHOD,
            "echo.agent/mcp/server_status",
            "wire method name pinned",
        );
        // The `forward_status` function uses
        // `mcp_dispatcher::SERVER_STATUS_METHOD` directly — same
        // const, no shadowing. If the import line at the top of
        // this file ever fans out a local copy, this test still
        // catches the wire name itself.
    }
}
