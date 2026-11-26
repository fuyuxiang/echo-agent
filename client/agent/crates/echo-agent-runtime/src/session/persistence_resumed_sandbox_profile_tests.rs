use super::{
    RelocationError, RelocationView, most_recent_local_summary_for_cwd_in_root,
    most_recent_local_summary_for_cwd_in_view, read_summary_from_dir,
    resumed_session_sandbox_profile_in_root,
};
use std::{fs, io};
use tempfile::TempDir;

/// Write a session summary under the *encoded* cwd dir (matching how the
/// resume helpers locate sessions). `sandbox_profile` is included only when
/// `Some`, mirroring older summaries that predate the field.
fn write_session(
    root: &std::path::Path,
    cwd: &str,
    session_id: &str,
    updated_at: &str,
    last_active_at: Option<&str>,
    sandbox_profile: Option<&str>,
    hidden: bool,
) {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join(session_id);
    fs::create_dir_all(&dir).unwrap();
    let mut summary = serde_json::json!({
        "info": { "id": session_id, "cwd": cwd },
        "session_summary": "",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": updated_at,
        "num_messages": 0,
        "current_model_id": "echo-agent-3",
    });
    if let Some(la) = last_active_at {
        summary["last_active_at"] = serde_json::Value::String(la.to_string());
    }
    if let Some(profile) = sandbox_profile {
        summary["sandbox_profile"] = serde_json::Value::String(profile.to_string());
    }
    if hidden {
        summary["hidden"] = serde_json::Value::Bool(true);
    }
    fs::write(dir.join("summary.json"), summary.to_string()).unwrap();
}

#[test]
fn explicit_id_returns_persisted_profile() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    write_session(
        &root,
        "/work/a",
        "sess-1",
        "2026-01-01T00:00:00Z",
        None,
        Some("strict"),
        false,
    );

    assert_eq!(
        resumed_session_sandbox_profile_in_root(Some("sess-1"), None, &root),
        Some("strict".to_string())
    );
}

#[test]
fn explicit_id_without_persisted_profile_is_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    // Older session, created before the field existed.
    write_session(
        &root,
        "/work/a",
        "sess-old",
        "2026-01-01T00:00:00Z",
        None,
        None,
        false,
    );

    assert_eq!(
        resumed_session_sandbox_profile_in_root(Some("sess-old"), None, &root),
        None
    );
}

#[test]
fn explicit_remote_id_resolves_local_child_profile() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/remote";
    // A remote session restored into a local child: the child has a fresh
    // id and records `parent_session_id` = the remote id.
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let dir = root.join(&encoded).join("local-child");
    fs::create_dir_all(&dir).unwrap();
    let summary = serde_json::json!({
        "info": { "id": "local-child", "cwd": cwd },
        "session_summary": "",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "num_messages": 0,
        "current_model_id": "echo-agent-3",
        "parent_session_id": "remote-xyz",
        "sandbox_profile": "workspace",
    });
    fs::write(dir.join("summary.json"), summary.to_string()).unwrap();

    // No session dir is named "remote-xyz"; resolve via the child (cwd-scoped).
    assert_eq!(
        resumed_session_sandbox_profile_in_root(Some("remote-xyz"), Some(cwd), &root),
        Some("workspace".to_string())
    );
    // Without a cwd the child can't be located -> None.
    assert_eq!(
        resumed_session_sandbox_profile_in_root(Some("remote-xyz"), None, &root),
        None
    );
}

#[test]
fn empty_or_missing_id_and_no_cwd_is_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    assert_eq!(
        resumed_session_sandbox_profile_in_root(Some(""), None, &root),
        None
    );
    assert_eq!(
        resumed_session_sandbox_profile_in_root(None, None, &root),
        None
    );
}

#[test]
fn most_recent_cwd_picks_latest_session_profile() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    write_session(
        &root,
        cwd,
        "older",
        "2026-01-01T00:00:00Z",
        None,
        Some("workspace"),
        false,
    );
    write_session(
        &root,
        cwd,
        "newer",
        "2026-06-01T00:00:00Z",
        None,
        Some("off"),
        false,
    );

    assert_eq!(
        most_recent_local_summary_for_cwd_in_root(cwd, &root)
            .unwrap()
            .info
            .id
            .0
            .to_string(),
        "newer"
    );
    assert_eq!(
        resumed_session_sandbox_profile_in_root(None, Some(cwd), &root),
        Some("off".to_string())
    );
}

#[test]
fn most_recent_cwd_skips_corrupt_summary() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    write_session(
        &root,
        cwd,
        "valid",
        "2026-06-01T00:00:00Z",
        None,
        Some("workspace"),
        false,
    );
    let corrupt_dir = root
        .join(crate::util::echo_agent_home::encode_cwd_dirname(cwd))
        .join("corrupt");
    fs::create_dir_all(&corrupt_dir).unwrap();
    fs::write(corrupt_dir.join("summary.json"), b"not-json").unwrap();

    let picked = most_recent_local_summary_for_cwd_in_root(cwd, &root).unwrap();
    assert_eq!(picked.info.id.0.as_ref(), "valid");
}

#[test]
fn most_recent_cwd_skips_raced_not_found() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    write_session(
        &root,
        cwd,
        "valid",
        "2026-06-01T00:00:00Z",
        None,
        Some("workspace"),
        false,
    );
    write_session(
        &root,
        cwd,
        "removed",
        "2026-07-01T00:00:00Z",
        None,
        Some("strict"),
        false,
    );
    let view = RelocationView::load_for_sessions_root(&root).unwrap();

    let picked = most_recent_local_summary_for_cwd_in_view(cwd, &view, |session_dir| {
        if session_dir.ends_with("removed") {
            Err(RelocationError::Io {
                operation: "read",
                path: session_dir.join("summary.json"),
                source: io::Error::new(io::ErrorKind::NotFound, "injected"),
            })
        } else {
            read_summary_from_dir(session_dir)
        }
    })
    .unwrap()
    .unwrap();
    assert_eq!(picked.info.id.0.as_ref(), "valid");
}

#[test]
fn most_recent_cwd_propagates_non_not_found_io_errors() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    write_session(
        &root,
        cwd,
        "older",
        "2026-01-01T00:00:00Z",
        None,
        Some("workspace"),
        false,
    );
    write_session(
        &root,
        cwd,
        "unreadable-newer",
        "2026-06-01T00:00:00Z",
        None,
        Some("strict"),
        false,
    );
    let view = RelocationView::load_for_sessions_root(&root).unwrap();

    let error = most_recent_local_summary_for_cwd_in_view(cwd, &view, |session_dir| {
        if session_dir.ends_with("unreadable-newer") {
            Err(RelocationError::Io {
                operation: "read",
                path: session_dir.join("summary.json"),
                source: io::Error::new(io::ErrorKind::PermissionDenied, "injected"),
            })
        } else {
            read_summary_from_dir(session_dir)
        }
    })
    .unwrap_err();
    assert!(matches!(
        error,
        RelocationError::Io { source, .. }
            if source.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn most_recent_cwd_prefers_last_active_at_over_updated_at() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    write_session(
        &root,
        cwd,
        "recent_activity",
        "2026-02-01T00:00:00Z",
        Some("2026-05-01T00:00:00Z"),
        Some("workspace"),
        false,
    );
    write_session(
        &root,
        cwd,
        "stale_activity",
        "2026-04-01T00:00:00Z",
        Some("2026-01-01T00:00:00Z"),
        Some("off"),
        false,
    );

    let picked = most_recent_local_summary_for_cwd_in_root(cwd, &root).unwrap();
    assert_eq!(picked.info.id.0.as_ref(), "recent_activity");
    assert_eq!(
        resumed_session_sandbox_profile_in_root(None, Some(cwd), &root),
        Some("workspace".to_string())
    );
}

#[test]
fn most_recent_cwd_skips_hidden_session() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    let cwd = "/work/proj";
    // Older, visible session.
    write_session(
        &root,
        cwd,
        "visible",
        "2026-01-01T00:00:00Z",
        None,
        Some("workspace"),
        false,
    );
    // Newer, hidden (e.g. subagent) session — the most-recent peek must
    // ignore it, matching what `list_sessions` resumes.
    write_session(
        &root,
        cwd,
        "hidden-newer",
        "2026-06-01T00:00:00Z",
        None,
        Some("off"),
        true,
    );

    assert_eq!(
        most_recent_local_summary_for_cwd_in_root(cwd, &root)
            .unwrap()
            .info
            .id
            .0
            .to_string(),
        "visible"
    );
    assert_eq!(
        resumed_session_sandbox_profile_in_root(None, Some(cwd), &root),
        Some("workspace".to_string())
    );
}

#[test]
fn most_recent_cwd_with_no_sessions_is_none() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("sessions");
    assert_eq!(
        resumed_session_sandbox_profile_in_root(None, Some("/empty/cwd"), &root),
        None
    );
}
