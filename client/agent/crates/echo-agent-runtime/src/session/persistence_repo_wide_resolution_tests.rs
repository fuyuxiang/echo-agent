use super::*;
use std::fs;

fn setup_session(root: &Path, cwd: &str, session_id: &str) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(session_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("summary.json"), b"{}").unwrap();
}

fn setup_child_session(root: &Path, cwd: &str, child_id: &str, parent_id: &str) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(child_id);
    fs::create_dir_all(&dir).unwrap();
    let summary = format!(
        r#"{{"session_id":"{child_id}","parent_session_id":"{parent_id}","updated_at":"2024-01-01T00:00:00Z"}}"#
    );
    fs::write(dir.join("summary.json"), summary).unwrap();
}

#[test]
fn exact_cwd_takes_priority_over_same_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";
    let other_cwd = "/repo/worktree-1";

    setup_session(&root, exact_cwd, "sess-A");
    setup_session(&root, other_cwd, "sess-A");

    let result = resolve_local_session_for_repo_in_root("sess-A", &[exact_cwd, other_cwd], &root);
    let r = result.unwrap();
    assert_eq!(r.session_id, "sess-A");
    assert_eq!(r.cwd, exact_cwd);
    assert_eq!(r.resolution_kind, LocalSessionResolutionKind::ExactCwd);
}

/// An `images/`-only stub in the exact cwd is skipped; resolution anchors to
/// the real session in a sibling cwd. Mirrors the cross-dir resume bug.
#[test]
fn skips_images_only_stub_and_resolves_real_sibling() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";
    let sibling_cwd = "/repo/worktree-1";

    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(exact_cwd);
    let images = root.join(&encoded).join("sess-A").join("images");
    fs::create_dir_all(&images).unwrap();
    fs::write(images.join("image-1.png"), b"png").unwrap();
    setup_session(&root, sibling_cwd, "sess-A");

    let result = resolve_local_session_for_repo_in_root("sess-A", &[exact_cwd, sibling_cwd], &root);
    let r = result.expect("must skip the stub and find the real sibling session");
    assert_eq!(r.cwd, sibling_cwd);
    assert_eq!(
        r.resolution_kind,
        LocalSessionResolutionKind::SameRepoDifferentCwd
    );
}

#[test]
fn falls_back_to_same_repo_cwd_when_not_in_exact() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";
    let other_cwd = "/repo/worktree-1";

    // Session only exists in other_cwd
    setup_session(&root, other_cwd, "sess-B");

    let result = resolve_local_session_for_repo_in_root("sess-B", &[exact_cwd, other_cwd], &root);
    let r = result.unwrap();
    assert_eq!(r.session_id, "sess-B");
    assert_eq!(r.cwd, other_cwd);
    assert_eq!(
        r.resolution_kind,
        LocalSessionResolutionKind::SameRepoDifferentCwd
    );
}

#[test]
fn finds_restored_child_in_exact_cwd() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";

    setup_child_session(&root, exact_cwd, "local-child", "remote-sess");

    let result = resolve_local_session_for_repo_in_root("remote-sess", &[exact_cwd], &root);
    let r = result.unwrap();
    assert_eq!(r.session_id, "local-child");
    assert_eq!(r.cwd, exact_cwd);
    assert_eq!(
        r.resolution_kind,
        LocalSessionResolutionKind::RestoredChildInExactCwd
    );
}

#[test]
fn finds_restored_child_in_same_repo_different_cwd() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";
    let other_cwd = "/repo/worktree-2";

    // Restored child only in other_cwd
    setup_child_session(&root, other_cwd, "restored-child", "remote-sess");

    let result =
        resolve_local_session_for_repo_in_root("remote-sess", &[exact_cwd, other_cwd], &root);
    let r = result.unwrap();
    assert_eq!(r.session_id, "restored-child");
    assert_eq!(r.cwd, other_cwd);
    assert_eq!(
        r.resolution_kind,
        LocalSessionResolutionKind::RestoredChildInSameRepoDifferentCwd
    );
}

#[test]
fn returns_none_when_no_candidate_has_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let result = resolve_local_session_for_repo_in_root(
        "nonexistent",
        &["/cwd-1", "/cwd-2", "/cwd-3"],
        &root,
    );
    assert!(result.is_none());
}

#[test]
fn direct_session_preferred_over_restored_child_in_same_cwd() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let cwd = "/repo/main";

    // Both exist: direct session AND a restored child for the same remote
    setup_session(&root, cwd, "sess-X");
    setup_child_session(&root, cwd, "child-of-X", "sess-X");

    let result = resolve_local_session_for_repo_in_root("sess-X", &[cwd], &root);
    let r = result.unwrap();
    // Direct match should win
    assert_eq!(r.session_id, "sess-X");
    assert_eq!(r.resolution_kind, LocalSessionResolutionKind::ExactCwd);
}

#[test]
fn direct_in_later_cwd_preferred_over_child_in_same_later_cwd() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let exact_cwd = "/repo/main";
    let other_cwd = "/repo/worktree-1";

    // Nothing in exact_cwd; both direct and child in other_cwd
    setup_session(&root, other_cwd, "sess-Y");
    setup_child_session(&root, other_cwd, "child-of-Y", "sess-Y");

    let result = resolve_local_session_for_repo_in_root("sess-Y", &[exact_cwd, other_cwd], &root);
    let r = result.unwrap();
    assert_eq!(r.session_id, "sess-Y");
    assert_eq!(
        r.resolution_kind,
        LocalSessionResolutionKind::SameRepoDifferentCwd
    );
}

#[test]
fn empty_candidates_returns_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let result = resolve_local_session_for_repo_in_root("any-sess", &[], &root);
    assert!(result.is_none());
}

#[test]
fn resolution_kind_serde_round_trip() {
    let kinds = [
        LocalSessionResolutionKind::ExactCwd,
        LocalSessionResolutionKind::RestoredChildInExactCwd,
        LocalSessionResolutionKind::SameRepoDifferentCwd,
        LocalSessionResolutionKind::RestoredChildInSameRepoDifferentCwd,
    ];
    for kind in &kinds {
        let json = serde_json::to_string(kind).unwrap();
        let deser: LocalSessionResolutionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(*kind, deser);
    }
}

#[test]
fn resolved_local_session_serde_round_trip() {
    let resolved = ResolvedLocalSession {
        session_id: "sess-123".into(),
        cwd: "/repo/main".into(),
        resolution_kind: LocalSessionResolutionKind::SameRepoDifferentCwd,
    };
    let json = serde_json::to_string(&resolved).unwrap();
    let deser: ResolvedLocalSession = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.session_id, "sess-123");
    assert_eq!(deser.cwd, "/repo/main");
    assert_eq!(
        deser.resolution_kind,
        LocalSessionResolutionKind::SameRepoDifferentCwd
    );
}
