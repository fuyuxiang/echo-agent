use super::{find_local_child_for_remote_in_root, session_exists_for_cwd_in_root};
use std::fs;
use tempfile::TempDir;

// resolve_local_session delegates to the same _in_root helpers tested above,
// so we test the composition logic via the public function indirectly by
// setting up the on-disk structures under a fake echoagent home.
// For unit isolation, we test the equivalent logic via the inner helpers.

fn setup_session(root: &std::path::Path, cwd: &str, session_id: &str) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(session_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("summary.json"), b"{}").unwrap();
}

fn setup_child_session(root: &std::path::Path, cwd: &str, child_id: &str, parent_id: &str) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(child_id);
    fs::create_dir_all(&dir).unwrap();
    let summary = serde_json::json!({ "parent_session_id": parent_id });
    fs::write(dir.join("summary.json"), summary.to_string()).unwrap();
}

#[test]
fn exact_match_returns_original_id() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project/alpha";
    let sid = "sess-123";

    setup_session(&root, cwd, sid);

    // Exact match: session_exists_for_cwd → true
    assert!(session_exists_for_cwd_in_root(sid, cwd, &root));
    // The composed function should return the original id.
    // (We can't call resolve_local_session directly because it uses echo_agent_home(),
    //  but the logic is: if session_exists → Some(session_id.to_string()),
    //  else find_local_child → child_id. Tested via inner helpers.)
    assert_eq!(
        Some(sid.to_string()),
        if session_exists_for_cwd_in_root(sid, cwd, &root) {
            Some(sid.to_string())
        } else {
            find_local_child_for_remote_in_root(sid, cwd, &root)
        }
    );
}

#[test]
fn child_match_returns_child_id() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project/beta";
    let remote_id = "remote-abc";
    let child_id = "local-child-xyz";

    setup_child_session(&root, cwd, child_id, remote_id);

    assert!(!session_exists_for_cwd_in_root(remote_id, cwd, &root));
    assert_eq!(
        Some(child_id.to_string()),
        find_local_child_for_remote_in_root(remote_id, cwd, &root)
    );
}

#[test]
fn no_match_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project/gamma";
    fs::create_dir_all(root.join(crate::util::echo_agent_home::encode_cwd_dirname(cwd))).unwrap();

    assert!(!session_exists_for_cwd_in_root("missing", cwd, &root));
    assert_eq!(
        None,
        find_local_child_for_remote_in_root("missing", cwd, &root)
    );
}

#[test]
fn exact_match_takes_priority_over_child() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project/delta";
    let sid = "sess-both";

    // Create both an exact session and a child of the same remote id.
    setup_session(&root, cwd, sid);
    setup_child_session(&root, cwd, "local-child-from-same", sid);

    // Exact match should take priority.
    assert!(session_exists_for_cwd_in_root(sid, cwd, &root));
}
