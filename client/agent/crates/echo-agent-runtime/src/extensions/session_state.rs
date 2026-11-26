//! `echo.agent/session/state` reads a session's metadata columns; `echo.agent/session/import`
//! writes them, with the transcript, to recreate a session on another host.

use std::path::{Path, PathBuf};

use agent_client_protocol as acp;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ExtResult;
use crate::session::persistence::Summary;
use crate::session::storage as st;

/// The summary column, required to load a session.
const SUMMARY_COLUMN: &str = "summary";

/// Logical column name to its file under the session directory. Paths come from the
/// storage layer so import and load never disagree about the on-disk layout. `summary`
/// is last so import writes it last, as the commit marker; keep it there.
const COLUMNS: &[(&str, &str)] = &[
    ("plan", st::PLAN_FILE),
    ("planMode", st::PLAN_MODE_FILE),
    ("signals", st::SIGNALS_FILE),
    ("goal", st::GOAL_STATE_FILE),
    ("announcement", st::ANNOUNCEMENT_STATE_FILE),
    (SUMMARY_COLUMN, st::SUMMARY_FILE),
];

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateRequest {
    session_id: String,
    cwd: String,
}

/// A session id is a UUID (see acp_agent's new_session); requiring that keeps it safe
/// to join into a filesystem path.
fn validate_session_uuid(session_id: &str) -> Result<(), acp::Error> {
    uuid::Uuid::try_parse(session_id)
        .map(|_| ())
        .map_err(|_| acp::Error::invalid_params().data("sessionId must be a UUID"))
}

/// `echo.agent/session/state`: return metadata columns keyed by logical name. Errors when
/// the session isn't found on this host, since it reads a single record whose absence
/// is not an empty result (unlike the collection returned by `echo.agent/session/updates`).
pub(crate) async fn handle_state(args: &acp::ExtRequest) -> ExtResult {
    let request: StateRequest = super::parse_params(args)?;
    validate_session_uuid(&request.session_id)?;

    let Some(dir) = resolve_session_dir(&request.session_id, &request.cwd) else {
        return Err(acp::Error::invalid_params().data("session not found"));
    };
    let mut state = serde_json::Map::new();
    for (column, rel) in COLUMNS {
        if let Ok(text) = std::fs::read_to_string(dir.join(rel))
            && let Ok(value) = serde_json::from_str::<Value>(&text)
        {
            state.insert((*column).to_string(), value);
        }
    }
    super::to_raw_response(&state)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportRequest {
    session_id: String,
    cwd: String,
    #[serde(default)]
    state: std::collections::HashMap<String, Value>,
    /// One JSON object per `updates.jsonl` line, not pre-serialized strings.
    #[serde(default)]
    updates: Vec<Value>,
}

/// `echo.agent/session/import`: recreate a session on this host from mirrored columns and
/// transcript. A session that already exists locally is left unchanged.
pub(crate) async fn handle_import(args: &acp::ExtRequest) -> ExtResult {
    let mut request: ImportRequest = super::parse_params(args)?;
    validate_session_uuid(&request.session_id)?;

    let info = crate::session::info::Info {
        id: acp::SessionId::new(request.session_id.clone()),
        cwd: request.cwd.clone(),
    };
    let dir = crate::session::persistence::session_dir(&info);

    // resolve_session_dir gates on summary.json, so an interrupted import (dir created,
    // summary not yet written) is recreated on retry rather than skipped forever.
    let has_local_session = resolve_session_dir(&request.session_id, &request.cwd).is_some();
    if !has_local_session {
        let Some(summary_value) = request.state.get_mut(SUMMARY_COLUMN) else {
            return Err(
                acp::Error::invalid_params().data("session/import requires a summary column")
            );
        };
        let Some(summary) = summary_value.as_object_mut() else {
            return Err(
                acp::Error::invalid_params().data("session/import summary must be an object")
            );
        };
        sanitize_summary_for_host(summary, &request.session_id, &request.cwd);
        // Reject a summary that would not load rather than persist one that bricks the
        // session and blocks re-import.
        if Summary::deserialize(&*summary_value).is_err() {
            return Err(acp::Error::invalid_params().data("summary column is not a valid summary"));
        }
        // Write the `.cwd` sidecar for hash-based (long-path) dirs so the session stays
        // recoverable by id, not just by (id, cwd).
        crate::util::echo_agent_home::ensure_sessions_cwd_dir(&request.cwd)
            .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
        write_import(&dir, &request.state, &request.updates)
            .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
    }
    super::to_raw_response(&json!({ "imported": !has_local_session }))
}

/// Rewrite a mirrored summary's host-specific fields to describe this host.
fn sanitize_summary_for_host(summary: &mut serde_json::Map<String, Value>, id: &str, cwd: &str) {
    if let Some(info_obj) = summary.get_mut("info").and_then(Value::as_object_mut) {
        info_obj.insert("id".to_string(), Value::String(id.to_string()));
        info_obj.insert("cwd".to_string(), Value::String(cwd.to_string()));
    }
    summary.insert(
        "chat_format_version".to_string(),
        json!(crate::session::persistence::CHAT_FORMAT_VERSION),
    );
    summary.insert("git_remotes".to_string(), json!([]));
    for field in [
        "prompt_display_cwd",
        "source_workspace_dir",
        "git_root_dir",
        "head_commit",
        "head_branch",
        "worktree_label",
        "request_id",
    ] {
        summary.remove(field);
    }
    set_or_remove(
        summary,
        "echo_agent_home",
        crate::session::persistence::echo_agent_home_string(),
    );
    set_or_remove(
        summary,
        "sandbox_profile",
        echo_agent_sandbox::configured_profile_name().map(String::from),
    );
}

fn set_or_remove(obj: &mut serde_json::Map<String, Value>, key: &str, value: Option<String>) {
    match value {
        Some(v) => {
            obj.insert(key.to_string(), Value::String(v));
        }
        None => {
            obj.remove(key);
        }
    }
}

/// Writes summary.json last, and each file to a temporary name first, so an interrupted
/// import leaves an incomplete session that load treats as absent.
fn write_import(
    dir: &Path,
    state: &std::collections::HashMap<String, Value>,
    updates: &[Value],
) -> std::io::Result<()> {
    crate::util::echo_agent_home::create_dir_all_owner_only(dir)?;

    // Clear every file this import owns so a leftover from a failed attempt can't
    // merge with the new snapshot; this import is authoritative.
    let _ = std::fs::remove_file(dir.join(st::CHAT_HISTORY_FILE));
    let _ = std::fs::remove_file(dir.join(st::UPDATES_FILE));
    for (_, rel) in COLUMNS {
        let _ = std::fs::remove_file(dir.join(rel));
    }

    if !updates.is_empty() {
        st::write_jsonl_atomic(&dir.join(st::UPDATES_FILE), updates)?;
    }

    for (column, rel) in COLUMNS {
        if let Some(value) = state.get(*column) {
            write_column(dir, rel, value)?;
        }
    }
    Ok(())
}

fn write_column(dir: &Path, rel: &str, value: &Value) -> std::io::Result<()> {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    st::write_bytes_atomic(&path, value.to_string().as_bytes())
}

/// The session's directory, or `None` when it isn't found on this host. Falls back to
/// an id scan when `(id, cwd)` has no summary (subagents use their own cwd); both
/// branches require summary.json so a bare directory doesn't count as present.
fn resolve_session_dir(session_id: &str, cwd: &str) -> Option<PathBuf> {
    let info = crate::session::info::Info {
        id: acp::SessionId::new(session_id.to_string()),
        cwd: cwd.to_string(),
    };
    let dir = crate::session::persistence::session_dir(&info);
    if dir.join(st::SUMMARY_FILE).is_file() {
        return Some(dir);
    }
    crate::session::persistence::find_session_dir_by_id(session_id)
        .filter(|found| found.join(st::SUMMARY_FILE).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_summary_for_host_rewrites_host_fields() {
        let mut summary = json!({
            "info": { "id": "s1", "cwd": "/remote/host/work" },
            "chat_format_version": 0,
            "prompt_display_cwd": "/remote/host/work",
            "source_workspace_dir": "/remote/host",
            "git_root_dir": "/remote/host/repo",
            "git_remotes": ["origin"],
            "head_commit": "deadbeef",
            "head_branch": "feature",
            "worktree_label": "wt",
            "request_id": "req-1",
        })
        .as_object()
        .unwrap()
        .clone();

        sanitize_summary_for_host(&mut summary, "s-new", "/local/work");

        assert_eq!(summary["info"]["id"], json!("s-new"));
        assert_eq!(summary["info"]["cwd"], json!("/local/work"));
        assert_eq!(
            summary["chat_format_version"],
            json!(crate::session::persistence::CHAT_FORMAT_VERSION)
        );
        assert_eq!(summary["git_remotes"], json!([]));
        for gone in [
            "prompt_display_cwd",
            "source_workspace_dir",
            "git_root_dir",
            "head_commit",
            "head_branch",
            "worktree_label",
            "request_id",
        ] {
            assert!(!summary.contains_key(gone), "{gone} should be dropped");
        }
    }

    #[test]
    fn write_import_writes_columns_updates_and_drops_stale_chat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("chat_history.jsonl"), b"stale cache").unwrap();
        // A column left by a failed prior import that the new payload omits.
        std::fs::write(dir.join("signals.json"), b"{\"stale\":true}").unwrap();

        let mut state = std::collections::HashMap::new();
        state.insert(
            "summary".to_string(),
            json!({ "info": { "id": "s1", "cwd": "/work" } }),
        );
        state.insert("plan".to_string(), json!({ "items": [] }));
        state.insert("goal".to_string(), json!({ "active": false }));
        let updates = vec![
            json!({ "method": "session/update", "params": { "a": 1 } }),
            json!({ "method": "session/update", "params": { "b": 2 } }),
        ];

        write_import(dir, &state, &updates).unwrap();

        #[cfg(unix)]
        assert_eq!(
            crate::test_support::unix_mode(dir),
            0o700,
            "imported session dir must be owner-only"
        );
        assert!(dir.join("summary.json").exists(), "summary.json written");
        assert_eq!(
            std::fs::read_to_string(dir.join("plan.json")).unwrap(),
            r#"{"items":[]}"#
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("goal/state.json")).unwrap(),
            r#"{"active":false}"#
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("updates.jsonl"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(
            !dir.join("chat_history.jsonl").exists(),
            "stale chat cache dropped so load rebuilds"
        );
        assert!(
            !dir.join("signals.json").exists(),
            "orphan column from a failed import dropped"
        );
    }
}
