use super::find_local_child_for_remote_in_root;
use filetime::{self, FileTime};
use std::fs;
use tempfile::TempDir;

fn make_session_with_parent(root: &std::path::Path, cwd: &str, session_id: &str, parent_id: &str) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(session_id);
    fs::create_dir_all(&dir).unwrap();
    let summary = serde_json::json!({ "parent_session_id": parent_id });
    fs::write(dir.join("summary.json"), summary.to_string()).unwrap();
}

#[test]
fn returns_child_id_when_parent_matches() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    make_session_with_parent(root.as_path(), "/work", "local-child-uuid", "remote-abc");

    let found = find_local_child_for_remote_in_root("remote-abc", "/work", &root);
    assert_eq!(found.as_deref(), Some("local-child-uuid"));
}

#[test]
fn returns_none_when_no_child_exists() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname("/work");
    fs::create_dir_all(root.join(&encoded)).unwrap();

    let found = find_local_child_for_remote_in_root("remote-abc", "/work", &root);
    assert!(found.is_none());
}

#[test]
fn returns_none_for_different_parent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    make_session_with_parent(root.as_path(), "/work", "local-child-uuid", "remote-xyz");

    let found = find_local_child_for_remote_in_root("remote-abc", "/work", &root);
    assert!(found.is_none());
}

/// Regression: a second `echoagent -r <remote_id>` must return the existing child
/// without creating a new restore, not return `None`.
#[test]
fn repeated_resume_returns_existing_child() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    make_session_with_parent(root.as_path(), "/project", "child-1", "remote-parent");

    let first = find_local_child_for_remote_in_root("remote-parent", "/project", &root);
    let second = find_local_child_for_remote_in_root("remote-parent", "/project", &root);
    assert_eq!(first, second);
    assert_eq!(first.as_deref(), Some("child-1"));
}

/// With multiple pre-existing children, the function must return the newest
/// one deterministically rather than picking an arbitrary filesystem order.
#[test]
fn duplicate_children_returns_newest_by_updated_at() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project";
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);

    // Older child — earlier timestamp.
    let old_dir = root.join(&encoded).join("old-child");
    fs::create_dir_all(&old_dir).unwrap();
    fs::write(
        old_dir.join("summary.json"),
        r#"{"parent_session_id":"remote-parent","updated_at":"2026-01-01T10:00:00Z"}"#,
    )
    .unwrap();

    // Newer child — later timestamp.
    let new_dir = root.join(&encoded).join("new-child");
    fs::create_dir_all(&new_dir).unwrap();
    fs::write(
        new_dir.join("summary.json"),
        r#"{"parent_session_id":"remote-parent","updated_at":"2026-06-01T10:00:00Z"}"#,
    )
    .unwrap();

    let found = find_local_child_for_remote_in_root("remote-parent", cwd, &root);
    assert_eq!(
        found.as_deref(),
        Some("new-child"),
        "must return the newest child by updated_at"
    );
}

/// When two children share the same `updated_at` the tie must be broken
/// deterministically, not by filesystem enumeration order.
/// The lexicographically largest session id is the final stable tie-breaker.
#[test]
fn duplicate_children_equal_timestamps_stable_tiebreak() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/project-tie";
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let same_ts = "2026-03-15T12:00:00Z";

    let mut dirs = Vec::new();
    for name in ["aaaa-uuid", "zzzz-uuid", "mmmm-uuid"] {
        let dir = root.join(&encoded).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("summary.json"),
            format!(r#"{{"parent_session_id":"remote-tie","updated_at":"{same_ts}"}}"#),
        )
        .unwrap();
        dirs.push(dir);
    }

    // Force all directories to have *exactly* the same mtime so the
    // lexicographic session_id comparison is the actual tie-breaker.
    // Without this, nanosecond-precision filesystem mtimes can differ.
    let fixed_mtime = FileTime::from_unix_time(1700000000, 0);
    for dir in &dirs {
        filetime::set_file_mtime(dir, fixed_mtime).unwrap();
    }

    let found = find_local_child_for_remote_in_root("remote-tie", cwd, &root);
    // All share the same updated_at and mtime.
    // The lexicographic tie-breaker must always pick "zzzz-uuid".
    assert_eq!(
        found.as_deref(),
        Some("zzzz-uuid"),
        "lexicographically largest id must win the three-way tie"
    );
}
