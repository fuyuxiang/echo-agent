use super::*;

#[test]
fn both_none_no_divergence() {
    assert!(detect_head_divergence(None, None, None).is_none());
}

#[test]
fn session_none_current_some_no_divergence() {
    assert!(detect_head_divergence(None, Some("main"), Some("abc123")).is_none());
}

#[test]
fn session_some_current_none_no_divergence() {
    assert!(detect_head_divergence(Some("abc123"), Some("main"), None).is_none());
}

#[test]
fn same_commit_no_divergence() {
    assert!(detect_head_divergence(Some("abc123"), Some("main"), Some("abc123")).is_none());
}

#[test]
fn different_commits_returns_divergence() {
    let d = detect_head_divergence(Some("abc123"), Some("feature/foo"), Some("def456"))
        .expect("should detect divergence");
    assert_eq!(d.session_commit, "abc123");
    assert_eq!(d.current_commit, "def456");
    assert_eq!(d.session_branch.as_deref(), Some("feature/foo"));
}

#[test]
fn different_commits_no_branch_returns_divergence() {
    let d = detect_head_divergence(Some("abc123"), None, Some("def456"))
        .expect("should detect divergence");
    assert_eq!(d.session_commit, "abc123");
    assert_eq!(d.current_commit, "def456");
    assert!(d.session_branch.is_none());
}

#[test]
fn serializes_to_camel_case_json() {
    let d = detect_head_divergence(Some("aaa"), Some("main"), Some("bbb")).unwrap();
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(json["sessionCommit"], "aaa");
    assert_eq!(json["currentCommit"], "bbb");
    assert_eq!(json["sessionBranch"], "main");
}

#[test]
fn serializes_without_branch_when_none() {
    let d = detect_head_divergence(Some("aaa"), None, Some("bbb")).unwrap();
    let json = serde_json::to_value(&d).unwrap();
    assert!(json.get("sessionBranch").is_none());
}
