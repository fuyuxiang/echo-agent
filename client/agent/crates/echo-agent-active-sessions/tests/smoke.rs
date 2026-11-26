//! End-to-end smoke test exercising the full active_sessions lifecycle.

use chrono::Utc;
use echo_agent_active_sessions::*;
use tempfile::TempDir;

fn session(id: &str, pid: u32) -> ActiveSession {
    ActiveSession {
        session_id: agent_client_protocol::SessionId::new(id),
        pid,
        cwd: "/tmp/test".into(),
        opened_at: Utc::now(),
    }
}

#[test]
fn full_lifecycle() {
    let dir = TempDir::new().unwrap();
    let r = dir.path();
    let pid = std::process::id();
    let sid = |s: &str| agent_client_protocol::SessionId::new(s);

    // Start session, verify listed.
    register_in(r, session("s1", pid)).unwrap();
    assert_eq!(list_in(r).unwrap().len(), 1);

    // Clean exit, verify gone.
    unregister_in(r, &sid("s1")).unwrap();
    assert!(list_in(r).unwrap().is_empty());

    // Simulate crash (dead PID) + live session.
    register_in(r, session("crashed", 2_000_000_000)).unwrap();
    register_in(r, session("alive", pid)).unwrap();

    // Crash detection finds dead PID, keeps live one.
    let crashed = collect_crashed_in(r).unwrap();
    assert_eq!(crashed.len(), 1);
    assert_eq!(&*crashed[0].session_id.0, "crashed");
    assert_eq!(list_in(r).unwrap().len(), 1);
}
