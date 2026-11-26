//! Registry-churn regression gate: a real in-process `MvpAgent` on duplex
//! ACP pipes churns sessions through create, prompt, and close, then
//! asserts via `echo.agent/debug/agent` that every registry count returns
//! to its pre-churn baseline. Deterministic counts, no memory thresholds.
//! Counts the echo workload never populates are pinned at their zero
//! baseline only.
mod acp_harness;
use acp_harness::{
    AutoApproveClient, connect_and_auth, ext_method, new_session, prompt_turn, run_agent_test,
};
use agent_client_protocol as acp;
use serde_json::json;
/// Enough that a per-cycle leak is unambiguous; well under a minute
/// against the loopback mock.
const CHURN_SESSIONS: usize = 15;
const CONCURRENT_SESSIONS: usize = 4;
/// Field names are the wire contract (`RegistrySnapshot` in
/// `agent/mvp_agent/session_lifecycle.rs`); `deny_unknown_fields` forces a
/// new server-side count to be mirrored and asserted here.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    sessions: usize,
    loading_sessions: usize,
    session_registry_entries: usize,
    session_threads: usize,
    resident_resources: usize,
    retained_resources: usize,
    dispatch_locks: usize,
    live_orphan_heal_locks: usize,
    session_turn_numbers: usize,
    permission_event_receivers: usize,
    model_unavailable_sessions: usize,
    session_live_state: usize,
    session_index_claims: usize,
    require_gateway_sessions: usize,
    subagent_pending: usize,
    subagent_active: usize,
    subagent_completed: usize,
    subagent_queued: usize,
    workspace_bindings: Option<usize>,
    workspace_activity_sessions: Option<usize>,
}
async fn read_counts(conn: &acp::ClientSideConnection) -> Counts {
    let resp = ext_method(conn, "echo.agent/debug/agent", json!({})).await;
    serde_json::from_value(resp["result"]["registries"].clone())
        .unwrap_or_else(|e| panic!("echo.agent/debug/agent: bad registries payload: {e}\n{resp}"))
}
/// Counts read once the actor threads are reaped. Nothing signals a thread
/// exit, so this polls; both ends settle, so neither catches one mid-exit.
async fn settled_counts(conn: &acp::ClientSideConnection) -> Counts {
    let mut counts = read_counts(conn).await;
    for _ in 0..100 {
        if counts.session_threads == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        counts = read_counts(conn).await;
    }
    counts
}
async fn close_session(conn: &acp::ClientSideConnection, session_id: &acp::SessionId) {
    let resp = ext_method(
        conn,
        "echo.agent/session/close",
        json!({ "sessionId": session_id.0.as_ref() }),
    )
    .await;
    assert_eq!(
        resp["result"]["success"],
        json!(true),
        "echo.agent/session/close on {} failed: {resp}",
        session_id.0
    );
}
async fn churn_one(conn: &acp::ClientSideConnection, cwd: &std::path::Path, label: usize) {
    let sid = new_session(conn, cwd).await;
    prompt_turn(conn, &sid, &format!("churn ping {label}")).await;
    close_session(conn, &sid).await;
}
#[test]
fn session_churn_returns_registry_snapshot_to_baseline() {
    run_agent_test(|cwd, _mock| async move {
        let conn = connect_and_auth(AutoApproveClient, "registry-churn-test")
            .await
            .0;
        churn_one(&conn, &cwd, 0).await;
        let baseline = settled_counts(&conn).await;
        assert_eq!(
            baseline.sessions, 0,
            "warmup session must be fully removed before baseline"
        );
        assert_eq!(
            (
                baseline.session_registry_entries,
                baseline.resident_resources,
                baseline.retained_resources,
                baseline.loading_sessions
            ),
            (0, 0, 0, 0),
            "warmup must leave no per-session resource entries, including \
             entries holding no resources"
        );
        assert_eq!(
            (
                baseline.workspace_bindings,
                baseline.workspace_activity_sessions
            ),
            (Some(0), Some(0)),
            "warmup must have built the local workspace and released both its \
             binding and its activity record"
        );
        assert_eq!(
            (
                baseline.subagent_pending,
                baseline.subagent_active,
                baseline.subagent_completed,
                baseline.subagent_queued
            ),
            (0, 0, 0, 0),
            "baseline must have no subagent entries"
        );
        for i in 1..=CHURN_SESSIONS {
            churn_one(&conn, &cwd, i).await;
        }
        let conn = &conn;
        let cwd = &cwd;
        let concurrent: Vec<acp::SessionId> =
            futures::future::join_all((0..CONCURRENT_SESSIONS).map(|_| new_session(conn, cwd)))
                .await;
        let mid = read_counts(conn).await;
        assert_eq!(
            mid.sessions, CONCURRENT_SESSIONS,
            "the snapshot must observe the open concurrent sessions"
        );
        futures::future::join_all(concurrent.iter().enumerate().map(|(i, sid)| async move {
            prompt_turn(conn, sid, &format!("concurrent ping {i}")).await;
        }))
        .await;
        futures::future::join_all(concurrent.iter().map(|sid| close_session(conn, sid))).await;
        let after = settled_counts(conn).await;
        assert_eq!(
            after, baseline,
            "session churn must return every registry count to baseline \
             (a growing count means a spawn-time map is missing its \
             remove_session release)"
        );
    });
}
