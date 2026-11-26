//! Three-tier semaphore admission + bounded-wait backpressure.
//!
//! Concurrent *running* calls are bounded at three scopes, acquired in a
//! fixed **session → connection → global** order. A consistent
//! most-local-first order is deadlock-free and never holds a scarce
//! global permit while blocking on a local one. A single shared deadline
//! spans all three acquisitions, so total admission latency is bounded by
//! `wait_timeout`, not `3 × wait_timeout`.
//!
//! Under moderate pressure `admit` waits; under very high pressure the
//! deadline elapses and the caller emits the shared overloaded JSON-RPC
//! error (`-32016` "tool_busy") instead of silently dropping the request.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use echo_agent_tool_protocol::{
    JsonRpcError, JsonRpcId, JsonRpcResponse, JsonRpcVersion, ResponseOutcome, SessionId,
};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Numeric JSON-RPC code for overload rejection (`echo-agent-tool-protocol`
/// `error_codes.rs`: `-32016` "tool_busy").
pub(crate) const TOOL_BUSY_CODE: i32 = -32016;

const TOOL_BUSY_MESSAGE: &str = "tool server busy; tool call rejected";

/// Default ceiling for the process-wide concurrency guard.
pub(crate) const DEFAULT_GLOBAL_MAX_INFLIGHT: usize = 1024;
/// Default per-session concurrent running calls.
pub(crate) const DEFAULT_SESSION_MAX_INFLIGHT: usize = 16;
/// Default per-connection concurrent running calls.
pub(crate) const DEFAULT_CONN_MAX_INFLIGHT: usize = 256;
/// Default bounded wait before an overloaded rejection.
pub(crate) const DEFAULT_ADMISSION_WAIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Ops-tunable override for the process-wide global cap (Helm `env:`).
const GLOBAL_MAX_INFLIGHT_ENV: &str = "ECHO_AGENT_TOOL_SERVER_GLOBAL_MAX_INFLIGHT";

/// Inflight-gauge scope labels, in acquisition order. A held [`AdmitGuard`]
/// counts against all three.
const SCOPES: [&str; 3] = ["session", "conn", "global"];

/// Build the shared overloaded (`-32016` "tool_busy") JSON-RPC error
/// response. This is the single source of the overload wire shape, reused
/// by BOTH the admission-timeout path (`server::execute_call`) and the
/// demux inbox-full path (`demux::route_session`) so the two never drift.
pub(crate) fn overloaded_response(id: JsonRpcId, session_id: SessionId) -> JsonRpcResponse<Value> {
    JsonRpcResponse {
        jsonrpc: JsonRpcVersion,
        id,
        session_id: Some(session_id),
        outcome: ResponseOutcome::Error(JsonRpcError {
            code: TOOL_BUSY_CODE,
            message: TOOL_BUSY_MESSAGE.to_owned(),
            data: Some(serde_json::json!({ "code": "tool_busy", "retryable": true })),
        }),
    }
}

/// Process-wide global admission semaphore, shared by every connection.
///
/// Initialized once at first use: the value comes from
/// `ECHO_AGENT_TOOL_SERVER_GLOBAL_MAX_INFLIGHT` when present and parseable as a
/// positive integer, otherwise `default_cap` (the builder knob, default
/// [`DEFAULT_GLOBAL_MAX_INFLIGHT`]). Because the cell initializes exactly
/// once, the first caller's `default_cap` and the env var at that instant
/// fix the process-wide capacity.
pub(crate) fn global_semaphore(default_cap: usize) -> Arc<Semaphore> {
    static SEM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| {
        let raw = std::env::var(GLOBAL_MAX_INFLIGHT_ENV).ok();
        Arc::new(Semaphore::new(resolve_global_cap(
            raw.as_deref(),
            default_cap,
        )))
    })
    .clone()
}

/// Resolve the process-wide global cap from the raw env value, falling
/// back to `default_cap`. Pure (no global state) so the
/// fall-back-never-panic guarantee is unit-tested: a non-numeric,
/// negative, empty, or zero value all yield `default_cap`.
fn resolve_global_cap(raw: Option<&str>, default_cap: usize) -> usize {
    raw.and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default_cap)
}

/// Why admission was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Overloaded {
    /// The bounded admission deadline elapsed under very high pressure.
    Timeout,
    /// A semaphore was closed — the server is shutting down.
    Shutdown,
}

/// RAII guard holding all three permits for the call's lifetime.
///
/// Fields drop in declaration order, so permits are released in reverse
/// of acquisition: global → connection → session.
#[derive(Debug)]
pub(crate) struct AdmitGuard {
    _global: OwnedSemaphorePermit,
    _conn: OwnedSemaphorePermit,
    _session: OwnedSemaphorePermit,
}

impl Drop for AdmitGuard {
    fn drop(&mut self) {
        for scope in SCOPES {
            crate::metrics::tool_call_inflight_dec(scope);
        }
    }
}

/// Three-tier admission controller. One per connection (`conn_sem`); the
/// per-session map is created/destroyed alongside each session loop.
#[derive(Debug)]
pub(crate) struct Admission {
    session_sems: DashMap<SessionId, Arc<Semaphore>>,
    session_max: usize,
    conn_sem: Arc<Semaphore>,
    global_sem: Arc<Semaphore>,
    wait_timeout: Duration,
}

impl Admission {
    pub(crate) fn new(
        session_max: usize,
        conn_max: usize,
        global_sem: Arc<Semaphore>,
        wait_timeout: Duration,
    ) -> Self {
        Self {
            session_sems: DashMap::new(),
            session_max,
            conn_sem: Arc::new(Semaphore::new(conn_max)),
            global_sem,
            wait_timeout,
        }
    }

    /// Create the per-session semaphore entry. Called from
    /// `bind_session_local` so the entry's lifetime is tied to the
    /// session-loop task, not lazily minted in [`Self::admit`].
    pub(crate) fn ensure_session(&self, session_id: &SessionId) {
        self.session_sems
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(self.session_max)));
    }

    /// Remove the per-session semaphore entry on unbind / loop exit.
    pub(crate) fn remove_session(&self, session_id: &SessionId) {
        self.session_sems.remove(session_id);
    }

    /// Acquire one permit at each scope (session → connection → global)
    /// against a single shared deadline.
    pub(crate) async fn admit(&self, session_id: &SessionId) -> Result<AdmitGuard, Overloaded> {
        let start = Instant::now();
        let deadline = start + self.wait_timeout;
        // The entry is created in `bind_session_local`; a straggler call
        // admitted just after unbind cleanup falls back to a private,
        // un-tracked semaphore rather than recreating a leaked entry.
        let session_sem = self
            .session_sems
            .get(session_id)
            .map(|s| s.clone())
            .unwrap_or_else(|| Arc::new(Semaphore::new(self.session_max)));

        let session = acquire_until(&session_sem, deadline).await?;
        let conn = acquire_until(&self.conn_sem, deadline).await?;
        let global = acquire_until(&self.global_sem, deadline).await?;

        crate::metrics::admission_wait_observe(start.elapsed().as_secs_f64());
        for scope in SCOPES {
            crate::metrics::tool_call_inflight_inc(scope);
        }
        Ok(AdmitGuard {
            _global: global,
            _conn: conn,
            _session: session,
        })
    }
}

/// Acquire one owned permit before `deadline`, mapping closed/elapsed to
/// the matching [`Overloaded`] variant.
async fn acquire_until(
    sem: &Arc<Semaphore>,
    deadline: Instant,
) -> Result<OwnedSemaphorePermit, Overloaded> {
    match tokio::time::timeout_at(deadline, sem.clone().acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_closed)) => Err(Overloaded::Shutdown),
        Err(_elapsed) => Err(Overloaded::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s).expect("valid session id")
    }

    fn test_admission(session_max: usize, conn_max: usize, global_max: usize) -> Admission {
        Admission::new(
            session_max,
            conn_max,
            Arc::new(Semaphore::new(global_max)),
            Duration::from_millis(150),
        )
    }

    #[test]
    fn resolve_global_cap_falls_back_on_bad_input_and_honors_valid() {
        // Absent / non-numeric / negative / empty / zero → default (never panic).
        assert_eq!(resolve_global_cap(None, 1024), 1024);
        assert_eq!(resolve_global_cap(Some("abc"), 1024), 1024);
        assert_eq!(resolve_global_cap(Some("-5"), 1024), 1024);
        assert_eq!(resolve_global_cap(Some(""), 1024), 1024);
        assert_eq!(resolve_global_cap(Some("0"), 1024), 1024);
        assert_eq!(resolve_global_cap(Some("  7"), 1024), 1024); // leading space → parse fails
        // A valid positive integer overrides the default.
        assert_eq!(resolve_global_cap(Some("2048"), 1024), 2048);
        assert_eq!(resolve_global_cap(Some("1"), 1024), 1);
    }

    #[test]
    fn overloaded_response_carries_minus_32016_and_data_marker() {
        let id: JsonRpcId = serde_json::from_value(serde_json::json!("call-1")).expect("id");
        let resp = overloaded_response(id, sid("s1"));
        let wire: Value = serde_json::from_str(&serde_json::to_string(&resp).expect("ser"))
            .expect("round-trips to json");
        assert_eq!(wire["error"]["code"], TOOL_BUSY_CODE);
        assert_eq!(wire["error"]["code"], -32016);
        assert_eq!(wire["error"]["data"]["code"], "tool_busy");
        assert_eq!(wire["error"]["data"]["retryable"], true);
        assert_eq!(wire["session_id"], "s1");
        assert_eq!(wire["id"], "call-1");
        assert!(
            wire.get("result").is_none(),
            "overload is an error, never a result"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn admit_times_out_when_session_saturated() {
        let admission = test_admission(2, 16, 64);
        let session = sid("sat");
        admission.ensure_session(&session);

        // Hold both session permits.
        let g1 = admission.admit(&session).await.expect("first admit");
        let _g2 = admission.admit(&session).await.expect("second admit");

        // Third admit must elapse the deadline → Timeout (not a hang).
        let result = admission.admit(&session).await;
        assert_eq!(result.unwrap_err(), Overloaded::Timeout);

        // Releasing one permit frees a slot for the next admit.
        drop(g1);
        admission
            .admit(&session)
            .await
            .expect("permit released → admit succeeds");
    }

    #[tokio::test(start_paused = true)]
    async fn admit_blocks_on_connection_scope_when_conn_saturated() {
        // conn_max = 1 is the binding constraint even though session has
        // room; a second admit on a *different* session still times out.
        let admission = test_admission(8, 1, 64);
        let a = sid("a");
        let b = sid("b");
        admission.ensure_session(&a);
        admission.ensure_session(&b);

        let _held = admission.admit(&a).await.expect("first admit");
        let result = admission.admit(&b).await;
        assert_eq!(
            result.unwrap_err(),
            Overloaded::Timeout,
            "connection cap binds across sessions"
        );
    }

    #[tokio::test]
    async fn admit_succeeds_repeatedly_under_capacity() {
        let admission = test_admission(4, 16, 64);
        let session = sid("ok");
        admission.ensure_session(&session);
        let mut guards = Vec::new();
        for _ in 0..4 {
            guards.push(admission.admit(&session).await.expect("within capacity"));
        }
        assert_eq!(guards.len(), 4);
    }

    #[tokio::test]
    async fn closed_semaphore_maps_to_shutdown() {
        let global = Arc::new(Semaphore::new(0));
        global.close();
        let admission = Admission::new(4, 16, global, Duration::from_secs(5));
        let session = sid("closed");
        admission.ensure_session(&session);
        let result = admission.admit(&session).await;
        assert_eq!(result.unwrap_err(), Overloaded::Shutdown);
    }

    #[tokio::test(start_paused = true)]
    async fn straggler_admit_after_remove_uses_private_permit() {
        let admission = test_admission(1, 16, 64);
        let session = sid("gone");
        // No ensure_session: simulate a straggler after unbind removed it.
        admission.remove_session(&session);
        // Falls back to a private semaphore and still admits (no panic,
        // no leaked tracked entry).
        let _g = admission.admit(&session).await.expect("private fallback");
        assert!(
            admission.session_sems.get(&session).is_none(),
            "straggler must not recreate a tracked entry"
        );
    }
}
