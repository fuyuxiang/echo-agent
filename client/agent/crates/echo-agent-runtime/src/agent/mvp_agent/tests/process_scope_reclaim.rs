//! Per-session process-tree reclaim on close, and cross-session isolation.
//!
//! Closing a session reaps the children enrolled in its scope; the reap runs on
//! the agent thread via `take_session`, so it works even if the actor wedged.
//! End-to-end reaping of specific subsystems is covered by their own soak tests.

use std::time::Duration;

use super::{build_minimal_agent_for_tests, make_test_handle, run_local_for_bridge_test};
use agent_client_protocol as acp;

fn sleeper() -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sleep");
    c.arg("600");
    c
}

async fn died(child: &mut tokio::process::Child) -> bool {
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_ok()
}

async fn still_running(child: &mut tokio::process::Child) -> bool {
    tokio::time::timeout(Duration::from_millis(500), child.wait())
        .await
        .is_err()
}

/// Register a session handle carrying `scope`, keyed by `sid`.
fn insert_session_with_scope(
    agent: &super::MvpAgent,
    sid: &acp::SessionId,
    scope: echo_agent_tty_utils::ProcessScope,
) {
    let mut handle = make_test_handle("test", false, None);
    handle.info.id = sid.clone();
    handle.tool_context.process_scope = Some(scope);
    agent.insert_resident(sid, handle);
}

#[test]
fn close_reaps_enrolled_session_child() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-scope-reclaim");
        let scope = echo_agent_tty_utils::ProcessScope::new();
        insert_session_with_scope(&agent, &sid, scope.clone());
        let (mut child, _owner) = scope.spawn(sleeper()).expect("spawn enrolled child");

        agent.remove_session(&sid);

        assert!(
            !agent.is_resident(&sid),
            "close must remove the session handle"
        );
        assert!(died(&mut child).await, "close must reap the enrolled child");
    });
}

#[test]
fn close_is_isolated_across_sessions() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid_a = acp::SessionId::new("sess-A");
        let sid_b = acp::SessionId::new("sess-B");
        let scope_a = echo_agent_tty_utils::ProcessScope::new();
        let scope_b = echo_agent_tty_utils::ProcessScope::new();
        insert_session_with_scope(&agent, &sid_a, scope_a.clone());
        insert_session_with_scope(&agent, &sid_b, scope_b.clone());
        let (mut child_a, _owner_a) = scope_a.spawn(sleeper()).expect("spawn A child");
        let (mut child_b, _owner_b) = scope_b.spawn(sleeper()).expect("spawn B child");

        agent.remove_session(&sid_a);

        assert!(died(&mut child_a).await, "closing A must reap A's child");
        assert!(
            still_running(&mut child_b).await,
            "closing A must not touch B's child"
        );

        agent.remove_session(&sid_b);
        assert!(died(&mut child_b).await, "closing B must reap B's child");
    });
}
