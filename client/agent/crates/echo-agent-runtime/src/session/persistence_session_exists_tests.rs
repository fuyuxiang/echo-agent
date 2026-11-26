use super::session_exists_in_root;
use std::fs;
use tempfile::TempDir;

fn make_root() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn returns_false_when_root_does_not_exist() {
    let root = std::path::PathBuf::from("/nonexistent/echoagent/sessions");
    assert!(!session_exists_in_root("any-id", &root));
}

#[test]
fn returns_false_when_root_is_empty() {
    let tmp = make_root();
    let root = tmp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    assert!(!session_exists_in_root("my-session", &root));
}

#[test]
fn returns_true_when_session_dir_exists_under_any_cwd() {
    let tmp = make_root();
    let root = tmp.path().join("sessions");
    // Simulate sessions/<encoded-cwd>/<session-id>/
    let session_dir = root.join("some_cwd_dir").join("my-session-id");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("summary.json"), b"{}").unwrap();

    assert!(session_exists_in_root("my-session-id", &root));
}

#[test]
fn returns_false_when_session_id_is_a_file_not_a_dir() {
    let tmp = make_root();
    let root = tmp.path().join("sessions");
    let cwd_dir = root.join("some_cwd_dir");
    fs::create_dir_all(&cwd_dir).unwrap();
    // Create a file instead of a directory with the session id name
    fs::write(cwd_dir.join("my-session-id"), b"").unwrap();

    assert!(!session_exists_in_root("my-session-id", &root));
}

#[test]
fn returns_false_for_different_session_id() {
    let tmp = make_root();
    let root = tmp.path().join("sessions");
    let session_dir = root.join("some_cwd_dir").join("session-a");
    fs::create_dir_all(&session_dir).unwrap();

    assert!(!session_exists_in_root("session-b", &root));
}

#[test]
fn finds_session_across_multiple_cwd_dirs() {
    let tmp = make_root();
    let root = tmp.path().join("sessions");
    // Two persisted sessions under different cwd directories.
    let other = root.join("cwd1").join("other-session");
    let target = root.join("cwd2").join("target-session");
    fs::create_dir_all(&other).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(other.join("summary.json"), b"{}").unwrap();
    fs::write(target.join("summary.json"), b"{}").unwrap();

    assert!(session_exists_in_root("target-session", &root));
    assert!(!session_exists_in_root("missing-session", &root));
}
