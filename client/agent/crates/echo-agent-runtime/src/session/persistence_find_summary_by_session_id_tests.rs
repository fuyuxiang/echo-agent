use super::find_summary_by_session_id_in_root;
use std::fs;
use tempfile::TempDir;

fn write_summary(root: &std::path::Path, cwd_dir: &str, session_id: &str, json: &str) {
    let dir = root.join(cwd_dir).join(session_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("summary.json"), json).unwrap();
}

fn minimal_summary(head_commit: &str, head_branch: &str) -> String {
    serde_json::json!({
        "info": { "id": "test-session", "cwd": "/tmp" },
        "session_summary": "",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "num_messages": 0,
        "current_model_id": "echo-agent-3",
        "head_commit": head_commit,
        "head_branch": head_branch
    })
    .to_string()
}

#[test]
fn returns_none_when_root_missing() {
    let result =
        find_summary_by_session_id_in_root("any", &std::path::PathBuf::from("/nonexistent"));
    assert!(result.is_none());
}

#[test]
fn returns_none_when_no_matching_session() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    write_summary(&root, "cwd1", "other-id", &minimal_summary("abc", "main"));
    assert!(find_summary_by_session_id_in_root("missing-id", &root).is_none());
}

#[test]
fn finds_summary_across_cwd_dirs() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    write_summary(
        &root,
        "encoded_cwd",
        "target-session",
        &minimal_summary("deadbeef", "feature/x"),
    );

    let found = find_summary_by_session_id_in_root("target-session", &root).unwrap();
    assert_eq!(found.head_commit.as_deref(), Some("deadbeef"));
    assert_eq!(found.head_branch.as_deref(), Some("feature/x"));
}

#[test]
fn skips_malformed_summary() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    // Write invalid JSON
    let dir = root.join("cwd1").join("bad-session");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("summary.json"), b"not-json").unwrap();

    assert!(find_summary_by_session_id_in_root("bad-session", &root).is_none());
}
