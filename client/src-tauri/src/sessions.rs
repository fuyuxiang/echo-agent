//! Session history discovery.
//!
//! EchoAgent persists sessions under `~/.echo-agent/sessions/<encoded-cwd>/<session-id>/`
//! with a `summary.json` in each. We list them (best-effort) for the sidebar.
//! The encoding of <encoded-cwd> is EchoAgent's `encode_cwd_dirname` (with a blake3
//! hash fallback for long paths); rather than reproduce that exactly we scan
//! ALL session directories and filter by the matching cwd inside summary.json.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_SUMMARY_BYTES: u64 = 1024 * 1024;
// Independent tasks may each own a cwd; keep the discovery budget aligned
// with the existing session budget so older tasks do not disappear early.
const MAX_CWD_DIRECTORIES: usize = 20_000;
const MAX_SESSION_DIRECTORIES: usize = 20_000;
const MAX_SESSION_RESULTS: usize = 10_000;
const MAX_WORKSPACE_RESULTS: usize = 512;
const MAX_TITLE_CHARS: usize = 512;
const MAX_CWD_CHARS: usize = 4_096;
const MAX_SESSION_ID_CHARS: usize = 256;
const MAX_MODEL_ID_CHARS: usize = 256;
const MAX_UPDATED_AT_CHARS: usize = 128;
const MAX_EXPERT_ID_CHARS: usize = 256;
const MAX_EXPERT_NAME_CHARS: usize = 512;
const MAX_EXPERT_AVATAR_CHARS: usize = 4_096;
const MAX_CWD_METADATA_BYTES: u64 = (MAX_CWD_CHARS * 4) as u64;
// One update line can contain the full 4 MiB prompt with JSON escaping. Read a
// bounded prefix only when an old automatic title was rejected as internal
// markup; ordinary session listing never touches the larger update log.
const MAX_TITLE_RECOVERY_PREFIX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_git_repo: Option<bool>,
    /// Pinned-to-top flag (EchoAgent-only state, NOT a EchoAgent field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// Archived (hidden from sidebar) flag (EchoAgent-only state, NOT a EchoAgent field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// Upstream session classification, e.g. `subagent` or `worktree`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    /// Effective upstream discovery visibility. Hidden child sessions remain
    /// addressable by id but must not appear as top-level tasks.
    #[serde(default)]
    pub hidden: bool,
    /// Model id bound to this session, if recorded in summary.json.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_model_id: Option<String>,
    /// Expert id bound to this session (EchoAgent-only state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expert_id: Option<String>,
    /// Expert display name (EchoAgent-only state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expert_name: Option<String>,
    /// Expert local avatar path (EchoAgent-only state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expert_avatar: Option<String>,
    /// Permission mode owned by this task. Legacy sessions default to Ask.
    pub permission_mode: String,
    /// Last renderer-observed turn lifecycle status (EchoAgent-only state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Subset of EchoAgent's `Summary` struct (see `echo-agent-runtime/src/session/persistence.rs:790`).
/// We only deserialize the fields we care about; unknown fields are ignored.
///
/// Display priority for `title` matches EchoAgent's own `display_title`:
/// `generated_title` (LLM-generated or manual /rename) > `session_summary`
/// (user's first message text).
#[derive(Debug, Deserialize)]
struct SummaryFile {
    /// User's first prompt text (legacy title field).
    #[serde(default, rename = "session_summary", alias = "summary")]
    summary: Option<String>,
    /// LLM-generated or manually-set title. Preferred over `summary`.
    #[serde(default)]
    generated_title: Option<String>,
    #[serde(default)]
    title_is_manual: Option<bool>,
    /// May live at the top level OR inside `info.id` — we read both.
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    /// EchoAgent bumps this on any activity; preferred when `updated_at` is stale.
    #[serde(default)]
    last_active_at: Option<String>,
    #[serde(default)]
    current_model_id: Option<String>,
    #[serde(default)]
    session_kind: Option<String>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    git_root_dir: Option<String>,
    /// Nested `info.id` / `info.cwd` shape (EchoAgent's Summary wraps these in Info).
    #[serde(default)]
    info: Option<SummaryInfo>,
    #[serde(default)]
    #[allow(dead_code)]
    mtime: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SummaryInfo {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

fn bounded_text(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect()
}

fn safe_session_id(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_SESSION_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(value.to_string())
}

fn read_summary_file(path: &Path) -> Option<SummaryFile> {
    let bytes = crate::shell_fs::read_regular_file_bounded(path, MAX_SUMMARY_BYTES).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn summary_cwd(summary: &SummaryFile) -> String {
    summary
        .cwd
        .clone()
        .or_else(|| summary.info.as_ref().and_then(|info| info.cwd.clone()))
        .unwrap_or_default()
}

fn canonical_workspace(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() || raw.chars().count() > MAX_CWD_CHARS {
        return None;
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    canonical.is_dir().then_some(canonical)
}

/// Recover a cwd from its authoritative directory when an early summary did
/// not persist cwd. Short paths are URL-encoded in the directory name; long
/// paths use a bounded `.cwd` sidecar. Never trust a decoded value unless
/// re-encoding it points back to the same directory.
fn cwd_from_directory(cwd_dir: &Path) -> Option<String> {
    let dirname = cwd_dir.file_name()?.to_str()?;
    let decoded = urlencoding::decode(dirname).ok()?.into_owned();
    let raw = if Path::new(&decoded).is_absolute() {
        decoded
    } else {
        let bytes = crate::shell_fs::read_regular_file_bounded(
            &cwd_dir.join(".cwd"),
            MAX_CWD_METADATA_BYTES,
        )
        .ok()?;
        String::from_utf8(bytes).ok()?.trim().to_string()
    };
    if raw.is_empty()
        || raw.chars().count() > MAX_CWD_CHARS
        || !Path::new(&raw).is_absolute()
        || !workspace_dir_matches_request(cwd_dir, &raw, canonical_workspace(&raw).as_deref())
    {
        return None;
    }
    Some(raw)
}

fn workspace_dir_matches_request(cwd_dir: &Path, raw_cwd: &str, canonical: Option<&Path>) -> bool {
    let Some(dirname) = cwd_dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if dirname == echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(raw_cwd) {
        return true;
    }
    canonical.is_some_and(|path| {
        dirname
            == echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(
                &path.to_string_lossy(),
            )
    })
}

fn authoritative_session_id(session_dir: &Path, summary: &SummaryFile) -> Option<String> {
    let directory_id = safe_session_id(session_dir.file_name()?.to_str()?.to_string())?;
    // A summary is data, not authority for locating a session. If it contains
    // an id it must agree with its enclosing directory; otherwise a copied or
    // forged summary could make workspace A authorize loading a session in B.
    for claimed in [
        summary.session_id.as_deref(),
        summary.info.as_ref().and_then(|info| info.id.as_deref()),
    ]
    .into_iter()
    .flatten()
    {
        if safe_session_id(claimed.to_string()).as_deref() != Some(directory_id.as_str()) {
            return None;
        }
    }
    Some(directory_id)
}

fn display_title(summary: &SummaryFile) -> Option<String> {
    let generated = summary
        .generated_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());

    // A manual rename is user-authored, so preserve it verbatim. Automatic
    // titles must pass the reasoning-markup filter, including legacy titles
    // already persisted before that filter existed.
    if summary.title_is_manual == Some(true) {
        if let Some(title) = generated {
            return Some(bounded_text(title.to_string(), MAX_TITLE_CHARS));
        }
    } else if let Some(title) = generated.and_then(crate::session_title::clean_auto_title) {
        return Some(bounded_text(title, MAX_TITLE_CHARS));
    }

    summary
        .summary
        .as_deref()
        .and_then(crate::session_title::clean_auto_title)
        .map(|title| bounded_text(title, MAX_TITLE_CHARS))
}

/// Recover a user-facing title for sessions written by builds that generated
/// an automatic title from injected expert context. `updates.jsonl` preserves
/// the compact ACP `displayText`; raw text is a compatibility fallback and is
/// stripped by `fallback_title_from_user_text` before use.
fn recover_title_from_updates(session_dir: &Path) -> Option<String> {
    let bytes = crate::shell_fs::read_regular_file_prefix(
        &session_dir.join("updates.jsonl"),
        MAX_TITLE_RECOVERY_PREFIX_BYTES,
    )
    .ok()?;

    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(update) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let Some(payload) = update.get("params").and_then(|params| params.get("update")) else {
            continue;
        };
        if payload
            .get("sessionUpdate")
            .and_then(serde_json::Value::as_str)
            != Some("user_message_chunk")
        {
            continue;
        }
        let Some(content) = payload.get("content") else {
            continue;
        };
        let display_text = content
            .get("_meta")
            .and_then(|meta| meta.get("displayText").or_else(|| meta.get("display_text")))
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty());
        let raw_text = content.get("text").and_then(serde_json::Value::as_str);
        if let Some(title) = display_text
            .or(raw_text)
            .and_then(crate::session_title::fallback_title_from_user_text)
        {
            return Some(bounded_text(title, MAX_TITLE_CHARS));
        }
    }
    None
}

fn to_session_summary(
    session_dir: &Path,
    summary: SummaryFile,
    cwd: String,
) -> Option<SessionSummary> {
    let session_id = authoritative_session_id(session_dir, &summary)?;
    let title = display_title(&summary)
        .or_else(|| recover_title_from_updates(session_dir))
        .unwrap_or_else(|| "未命名会话".into());
    let updated_at = summary
        .updated_at
        .clone()
        .or_else(|| summary.last_active_at.clone())
        .map(|value| bounded_text(value, MAX_UPDATED_AT_CHARS));
    let is_git_repo = summary.git_root_dir.as_ref().map(|path| !path.is_empty());
    let hidden = session_is_hidden(&summary, &session_id);
    Some(SessionSummary {
        session_id,
        title,
        updated_at,
        cwd,
        is_git_repo,
        pinned: None,
        archived: None,
        session_kind: summary
            .session_kind
            .map(|value| bounded_text(value, MAX_MODEL_ID_CHARS)),
        hidden,
        current_model_id: summary
            .current_model_id
            .map(|value| bounded_text(value, MAX_MODEL_ID_CHARS)),
        expert_id: None,
        expert_name: None,
        expert_avatar: None,
        permission_mode: "ask".into(),
        status: None,
    })
}

fn summary_is_hidden(summary: &SummaryFile) -> bool {
    summary.hidden.unwrap_or_else(|| {
        summary
            .session_kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with("subagent"))
    })
}

fn session_is_hidden(summary: &SummaryFile, session_id: &str) -> bool {
    summary_is_hidden(summary) || crate::groups::is_member_session(session_id)
}

fn later_timestamp(current: Option<&str>, candidate: &str) -> bool {
    let Ok(candidate) = chrono::DateTime::parse_from_rfc3339(candidate) else {
        return false;
    };
    current
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .is_none_or(|current| candidate > current)
}

fn apply_metadata_and_sort(out: &mut Vec<SessionSummary>, include_archived: bool) {
    // Merge EchoAgent-only pinned/archived state (sidecar file, since EchoAgent's
    // Summary has no such fields and would clobber any we tried to add).
    let state = crate::meta::read_state();
    apply_metadata_from_state(out, include_archived, &state);
}

fn apply_metadata_from_state(
    out: &mut Vec<SessionSummary>,
    include_archived: bool,
    state: &crate::meta::EchoAgentState,
) {
    let pinned = state.pinned_set();
    let archived = state.archived_set();
    let experts = state.expert_map();
    let statuses = state.session_status_map();
    // Keep the historical default (hide archived), while allowing the archive
    // view to request the same authoritative rows with `archived: true`.
    apply_archive_visibility(out, &archived, include_archived);
    for entry in out.iter_mut() {
        entry.pinned = Some(pinned.contains(&entry.session_id));
        if let Some(binding) = experts.get(&entry.session_id) {
            entry.expert_id = Some(bounded_text(binding.expert_id.clone(), MAX_EXPERT_ID_CHARS));
            entry.expert_name = Some(bounded_text(
                binding.expert_name.clone(),
                MAX_EXPERT_NAME_CHARS,
            ));
            entry.expert_avatar = binding
                .avatar_local
                .clone()
                .map(|value| bounded_text(value, MAX_EXPERT_AVATAR_CHARS));
        }
        entry.permission_mode = state
            .permission_mode_map()
            .get(&entry.session_id)
            .cloned()
            .unwrap_or_else(|| "ask".into());
        if let Some(lifecycle) = statuses.get(&entry.session_id) {
            entry.status = Some(lifecycle.status.clone());
            if later_timestamp(entry.updated_at.as_deref(), &lifecycle.updated_at) {
                entry.updated_at = Some(lifecycle.updated_at.clone());
            }
        }
    }
    // Sort: pinned first, then by updated_at descending (falling back to the
    // session_id, which is a UUIDv7 — roughly chronological).
    out.sort_by(|a, b| {
        b.pinned
            .unwrap_or(false)
            .cmp(&a.pinned.unwrap_or(false))
            .then_with(|| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| b.session_id.cmp(&a.session_id))
            })
    });
}

/// List sessions for a given cwd. Reads `~/.echo-agent/sessions/**/*.json` and
/// filters by cwd. Best-effort: missing/invalid entries are skipped.
pub fn list_sessions(cwd: &str, include_archived: bool) -> Vec<SessionSummary> {
    let sessions_root = agent_sessions_root();
    list_sessions_from_root(&sessions_root, cwd, include_archived)
}

fn list_sessions_from_root(
    sessions_root: &Path,
    cwd: &str,
    include_archived: bool,
) -> Vec<SessionSummary> {
    let mut out = Vec::new();
    let requested_canonical = canonical_workspace(cwd);
    let output_cwd = requested_canonical
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| bounded_text(cwd.to_string(), MAX_CWD_CHARS));
    let mut visited_sessions = 0_usize;

    let Ok(cwd_dirs) = std::fs::read_dir(sessions_root) else {
        return out;
    };
    for cwd_entry in cwd_dirs.flatten().take(MAX_CWD_DIRECTORIES) {
        let cwd_path = cwd_entry.path();
        if !cwd_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(session_dirs) = std::fs::read_dir(&cwd_path) else {
            continue;
        };
        for sess_entry in session_dirs.flatten() {
            if visited_sessions >= MAX_SESSION_DIRECTORIES || out.len() >= MAX_SESSION_RESULTS {
                break;
            }
            visited_sessions += 1;
            let sess_path = sess_entry.path();
            if !sess_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let summary_path = sess_path.join("summary.json");
            let Some(s) = read_summary_file(&summary_path) else {
                continue;
            };
            // A present cwd must resolve to the requested workspace. Legacy
            // summaries without cwd are accepted only from the exact encoded
            // directory for the requested raw/canonical path; absence must
            // never become a wildcard across every workspace.
            let entry_cwd = summary_cwd(&s);
            if entry_cwd.is_empty() {
                if !workspace_dir_matches_request(&cwd_path, cwd, requested_canonical.as_deref()) {
                    continue;
                }
            } else {
                let entry_canonical = canonical_workspace(&entry_cwd);
                if !workspace_dir_matches_request(&cwd_path, &entry_cwd, entry_canonical.as_deref())
                {
                    continue;
                }
                let same_workspace = match (requested_canonical.as_ref(), entry_canonical.as_ref())
                {
                    (Some(requested), Some(entry)) => requested == entry,
                    _ => entry_cwd == cwd,
                };
                if !same_workspace {
                    continue;
                }
            }
            if let Some(summary) = to_session_summary(&sess_path, s, output_cwd.clone()) {
                out.push(summary);
            }
        }
        if visited_sessions >= MAX_SESSION_DIRECTORIES || out.len() >= MAX_SESSION_RESULTS {
            break;
        }
    }
    apply_metadata_and_sort(&mut out, include_archived);
    out
}

/// List sessions from every persisted working directory in one bounded scan.
/// A task's cwd remains its execution context; it is not a presentation group.
/// This lets upgraded clients recover sessions created under an older default
/// cwd without granting an arbitrary caller the ability to choose a directory.
pub fn list_all_sessions(include_archived: bool) -> Result<Vec<SessionSummary>, String> {
    let sessions_root = agent_sessions_root();
    let mut out = list_all_sessions_from_root(&sessions_root)?;
    apply_metadata_and_sort(&mut out, include_archived);
    Ok(out)
}

/// Confirm that a persisted session record exists before writing sidecar
/// metadata such as pin/archive flags. This prevents stale or forged renderer
/// ids from creating permanent ghost entries in `echoagent-state.json`.
pub fn persisted_session_exists(session_id: &str) -> Result<bool, String> {
    let sessions_root = agent_sessions_root();
    persisted_session_exists_from_root(&sessions_root, session_id)
}

fn persisted_session_exists_from_root(
    sessions_root: &Path,
    session_id: &str,
) -> Result<bool, String> {
    if session_id.trim().is_empty()
        || session_id.chars().count() > MAX_SESSION_ID_CHARS
        || session_id.chars().any(char::is_control)
    {
        return Ok(false);
    }
    Ok(list_all_sessions_from_root(sessions_root)?
        .iter()
        .any(|summary| summary.session_id == session_id))
}

pub(crate) fn list_all_sessions_from_root(
    sessions_root: &Path,
) -> Result<Vec<SessionSummary>, String> {
    let mut out = Vec::new();
    let mut visited_sessions = 0_usize;

    let cwd_dirs = match std::fs::read_dir(sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(format!("读取历史任务目录失败：{error}")),
    };
    for cwd_entry in cwd_dirs.flatten().take(MAX_CWD_DIRECTORIES) {
        let cwd_path = cwd_entry.path();
        if !cwd_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(session_dirs) = std::fs::read_dir(&cwd_path) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            if visited_sessions >= MAX_SESSION_DIRECTORIES || out.len() >= MAX_SESSION_RESULTS {
                break;
            }
            visited_sessions += 1;
            let session_path = session_entry.path();
            if !session_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(summary) = read_summary_file(&session_path.join("summary.json")) else {
                continue;
            };
            // Early summaries may omit cwd. Recover it only from the
            // authoritative encoded directory (and its bounded `.cwd` sidecar).
            let raw_cwd = match summary_cwd(&summary) {
                value if !value.is_empty() => value,
                _ => match cwd_from_directory(&cwd_path) {
                    Some(value) => value,
                    None => continue,
                },
            };
            if raw_cwd.is_empty()
                || raw_cwd.chars().count() > MAX_CWD_CHARS
                || !Path::new(&raw_cwd).is_absolute()
            {
                continue;
            }
            let canonical = canonical_workspace(&raw_cwd);
            if !workspace_dir_matches_request(&cwd_path, &raw_cwd, canonical.as_deref()) {
                continue;
            }
            let output_cwd = canonical
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| bounded_text(raw_cwd, MAX_CWD_CHARS));
            if let Some(summary) = to_session_summary(&session_path, summary, output_cwd) {
                out.push(summary);
            }
        }
        if visited_sessions >= MAX_SESSION_DIRECTORIES || out.len() >= MAX_SESSION_RESULTS {
            break;
        }
    }

    Ok(out)
}

fn apply_archive_visibility(
    sessions: &mut Vec<SessionSummary>,
    archived: &std::collections::HashSet<String>,
    include_archived: bool,
) {
    if !include_archived {
        sessions.retain(|entry| !archived.contains(&entry.session_id));
    }
    for entry in sessions {
        entry.archived = Some(archived.contains(&entry.session_id));
    }
}

/// A discovered workspace (working directory EchoAgent has run sessions in).
/// Used to populate the Composer's working-directory dropdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    /// Absolute path of the working directory.
    pub cwd: String,
    /// Number of sessions recorded under this cwd.
    pub session_count: usize,
    /// Title of the most recent session under this cwd (for display).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_title: Option<String>,
}

/// Scan `~/.echo-agent/sessions/**/summary.json` and collapse the results by cwd.
/// Unlike `list_sessions` (which filters to one cwd), this returns every cwd
/// EchoAgent has ever seen, deduplicated, with a session count per cwd. Used to
/// populate the workspace picker. Best-effort: malformed entries are skipped.
pub fn list_workspaces() -> Vec<WorkspaceInfo> {
    let sessions_root = agent_sessions_root();
    list_workspaces_from_root(&sessions_root)
}

fn list_workspaces_from_root(sessions_root: &Path) -> Vec<WorkspaceInfo> {
    let default_workspace = crate::paths::default_workspace_dir().ok();
    list_workspaces_from_root_with_default(sessions_root, default_workspace.as_deref())
}

fn list_workspaces_from_root_with_default(
    sessions_root: &Path,
    default_workspace: Option<&Path>,
) -> Vec<WorkspaceInfo> {
    // cwd -> (count, last_title)
    let mut map: std::collections::HashMap<String, (usize, Option<String>)> =
        std::collections::HashMap::new();
    let mut visited_sessions = 0_usize;

    let Ok(cwd_dirs) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    for cwd_entry in cwd_dirs.flatten().take(MAX_CWD_DIRECTORIES) {
        let cwd_path = cwd_entry.path();
        if !cwd_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(session_dirs) = std::fs::read_dir(&cwd_path) else {
            continue;
        };
        for sess_entry in session_dirs.flatten() {
            if visited_sessions >= MAX_SESSION_DIRECTORIES {
                break;
            }
            visited_sessions += 1;
            if !sess_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let session_path = sess_entry.path();
            let summary_path = session_path.join("summary.json");
            let Some(s) = read_summary_file(&summary_path) else {
                continue;
            };
            let Some(session_id) = authoritative_session_id(&session_path, &s) else {
                continue;
            };
            if session_is_hidden(&s, &session_id) {
                continue;
            }
            let title = display_title(&s).or_else(|| recover_title_from_updates(&session_path));
            let claimed_cwd = summary_cwd(&s);
            let raw_entry_cwd = if claimed_cwd.is_empty() {
                match cwd_from_directory(&cwd_path) {
                    Some(value) => value,
                    None => continue,
                }
            } else {
                claimed_cwd
            };
            let Some(entry_cwd) = canonical_workspace(&raw_entry_cwd) else {
                continue;
            };
            if default_workspace
                .is_some_and(|base| crate::session_workspace::is_task_directory(base, &entry_cwd))
            {
                continue;
            }
            if !workspace_dir_matches_request(&cwd_path, &raw_entry_cwd, Some(entry_cwd.as_path()))
            {
                continue;
            }
            let entry_cwd = entry_cwd.to_string_lossy().into_owned();
            if !map.contains_key(&entry_cwd) && map.len() >= MAX_WORKSPACE_RESULTS {
                continue;
            }
            let entry = map.entry(entry_cwd).or_insert((0, None));
            entry.0 += 1;
            // Keep the last non-empty summary as the display title.
            if let Some(title) = title {
                entry.1 = Some(title);
            }
        }
        if visited_sessions >= MAX_SESSION_DIRECTORIES {
            break;
        }
    }

    let mut out: Vec<WorkspaceInfo> = map
        .into_iter()
        .map(|(cwd, (session_count, last_title))| WorkspaceInfo {
            cwd,
            session_count,
            last_title,
        })
        .collect();
    // Busiest workspaces first (most sessions), tie-break alphabetically.
    out.sort_by(|a, b| {
        b.session_count
            .cmp(&a.session_count)
            .then(a.cwd.cmp(&b.cwd))
    });
    out.truncate(MAX_WORKSPACE_RESULTS);
    out
}

fn agent_sessions_root() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("sessions")
}

/// Run only before constructing Runtime actors. Live summaries are owned by
/// their actors and must never be rewritten behind Runtime's back.
pub(crate) fn migrate_builtin_models_before_runtime() -> Result<usize, String> {
    migrate_builtin_models_from_root(&agent_sessions_root())
}

/// Files restored or made readable after startup must not reach Runtime's
/// unrelated-provider fallback. A restart safely migrates them before loading.
pub(crate) fn require_migrated_model(session_id: &str, cwd: &Path) -> Result<(), String> {
    require_migrated_model_from_root(&agent_sessions_root(), session_id, cwd)
}

fn require_migrated_model_from_root(
    root: &Path,
    session_id: &str,
    cwd: &Path,
) -> Result<(), String> {
    if list_sessions_from_root(root, &cwd.to_string_lossy(), true)
        .iter()
        .any(|summary| {
            summary.session_id == session_id
                && summary
                    .current_model_id
                    .as_deref()
                    .is_some_and(crate::providers::is_legacy_builtin_model_id)
        })
    {
        return Err(
            "此任务的旧模型配置尚未完成升级，请重启 EchoAgent 后重新打开；已有对话和文件会保留。"
                .into(),
        );
    }
    Ok(())
}

fn migrate_builtin_models_from_root(root: &Path) -> Result<usize, String> {
    let directories = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取旧任务模型配置失败：{error}")),
    };
    let mut migrated = 0;
    for directory in directories {
        let directory = directory.map_err(|error| error.to_string())?;
        if !directory.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for entry in std::fs::read_dir(directory.path()).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path().join("summary.json");
            if !std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            {
                continue;
            }
            let bytes = match crate::shell_fs::read_regular_file_bounded(&path, MAX_SUMMARY_BYTES) {
                Ok(bytes) => bytes,
                Err(error) => {
                    // Discovery already excludes these records. One unrelated
                    // unreadable/oversized history must not disable all chat.
                    tracing::warn!(path = %path.display(), %error, "skip unreadable history during model migration");
                    continue;
                }
            };
            let Ok(summary) = serde_json::from_slice::<SummaryFile>(&bytes) else {
                continue;
            };
            if authoritative_session_id(&entry.path(), &summary).is_none() {
                continue;
            }
            if !summary
                .current_model_id
                .as_deref()
                .is_some_and(crate::providers::is_legacy_builtin_model_id)
            {
                continue;
            }
            // Preserve fields owned by newer Runtime versions, including all
            // history, timestamps, usage, and workspace identity.
            let mut value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            crate::providers::migrate_builtin_model_reference(&mut value["current_model_id"]);
            let updated = serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?;
            crate::paths::write_private_file(&path, &updated)?;
            migrated += 1;
        }
    }
    Ok(migrated)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_archive_visibility, apply_metadata_from_state, authoritative_session_id,
        display_title, list_all_sessions_from_root, list_sessions_from_root,
        list_workspaces_from_root, persisted_session_exists_from_root, read_summary_file,
        recover_title_from_updates, summary_cwd, to_session_summary, workspace_dir_matches_request,
        SessionSummary, SummaryFile, MAX_SUMMARY_BYTES, MAX_TITLE_CHARS,
    };

    #[test]
    fn startup_migrates_saved_models_without_rewriting_history_or_unknown_fields() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("sessions");
        let workspace = root.join("workspace");
        let models = [
            "echoagent-ojlab/chat-xc",
            "echoagent-ojlab/chat-glm",
            "echoagent-ojlab/chat-qwen",
            "mine/chat-qwen",
            "echoagent-ojlab/Echo Grove",
        ];
        for (index, model) in models.iter().enumerate() {
            let id = format!("saved-{index}");
            let directory = workspace.join(&id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("summary.json"),
                serde_json::to_vec(&serde_json::json!({
                    "info":{"id":id,"cwd":"/saved"}, "current_model_id":model,
                    "generated_title":"旧 chat-qwen 对话", "future":{"unchanged":true}
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::write(
                directory.join("updates.jsonl"),
                b"historical model: chat-qwen\n",
            )
            .unwrap();
        }
        assert_eq!(super::migrate_builtin_models_from_root(&root).unwrap(), 3);
        for (index, model) in models.iter().enumerate() {
            let directory = workspace.join(format!("saved-{index}"));
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(directory.join("summary.json")).unwrap())
                    .unwrap();
            assert_eq!(
                value["current_model_id"].as_str(),
                Some(if index < 3 {
                    crate::providers::BUILTIN_DEFAULT_MODEL_ID
                } else {
                    model
                })
            );
            assert_eq!(value["future"]["unchanged"], true);
            assert_eq!(value["generated_title"], "旧 chat-qwen 对话");
            assert_eq!(
                std::fs::read(directory.join("updates.jsonl")).unwrap(),
                b"historical model: chat-qwen\n"
            );
        }
        assert_eq!(super::migrate_builtin_models_from_root(&root).unwrap(), 0);
    }

    #[test]
    fn late_imported_legacy_task_is_blocked_until_startup_migration() {
        let home = tempfile::tempdir().unwrap();
        let cwd = home.path().canonicalize().unwrap();
        let root = home.path().join("sessions");
        let directory = root
            .join(
                echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(
                    &cwd.to_string_lossy(),
                ),
            )
            .join("old-task");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("summary.json"),
            serde_json::to_vec(&serde_json::json!({
                "info":{"id":"old-task", "cwd":cwd},"current_model_id":"echoagent-ojlab/chat-xc"
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            super::require_migrated_model_from_root(&root, "old-task", &cwd)
                .unwrap_err()
                .contains("重启")
        );
        assert!(super::require_migrated_model_from_root(&root, "unrelated-task", &cwd).is_ok());
        assert_eq!(super::migrate_builtin_models_from_root(&root).unwrap(), 1);
        assert!(super::require_migrated_model_from_root(&root, "old-task", &cwd).is_ok());
    }

    #[test]
    fn an_unrelated_oversized_history_does_not_prevent_startup_migration() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("workspace/too-large");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("summary.json"),
            vec![b' '; MAX_SUMMARY_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            super::migrate_builtin_models_from_root(home.path()).unwrap(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_migration_does_not_follow_linked_summaries_or_directories() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let contents = br#"{"current_model_id":"echoagent-ojlab/chat-qwen"}"#;
        let path = outside.path().join("summary.json");
        std::fs::write(&path, contents).unwrap();
        let directory = home.path().join("workspace/task");
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(&path, directory.join("summary.json")).unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("workspace/linked-task"))
            .unwrap();
        assert_eq!(
            super::migrate_builtin_models_from_root(home.path()).unwrap(),
            0
        );
        assert_eq!(std::fs::read(path).unwrap(), contents);
    }

    fn summary(json: &str) -> SummaryFile {
        serde_json::from_str(json).expect("valid summary fixture")
    }

    fn session(id: &str) -> SessionSummary {
        SessionSummary {
            session_id: id.into(),
            title: id.into(),
            updated_at: None,
            cwd: "/tmp".into(),
            is_git_repo: None,
            pinned: None,
            archived: None,
            session_kind: None,
            hidden: false,
            current_model_id: None,
            expert_id: None,
            expert_name: None,
            expert_avatar: None,
            permission_mode: "ask".into(),
            status: None,
        }
    }

    #[test]
    fn archived_sessions_are_opt_in_and_marked_authoritatively() {
        let archived = std::collections::HashSet::from(["archived".to_string()]);
        let mut default_rows = vec![session("active"), session("archived")];
        apply_archive_visibility(&mut default_rows, &archived, false);
        assert_eq!(default_rows.len(), 1);
        assert_eq!(default_rows[0].archived, Some(false));

        let mut all_rows = vec![session("active"), session("archived")];
        apply_archive_visibility(&mut all_rows, &archived, true);
        assert_eq!(all_rows.len(), 2);
        assert_eq!(all_rows[1].archived, Some(true));
    }

    #[test]
    fn lifecycle_metadata_is_merged_and_advances_catalog_activity_time() {
        let mut state = crate::meta::EchoAgentState::default();
        state.session_statuses.insert(
            "active".into(),
            crate::meta::SessionLifecycle {
                status: "awaiting_permission".into(),
                updated_at: "2026-09-14T09:00:00Z".into(),
            },
        );
        let mut rows = vec![SessionSummary {
            updated_at: Some("2026-09-14T08:00:00Z".into()),
            ..session("active")
        }];

        apply_metadata_from_state(&mut rows, false, &state);

        assert_eq!(rows[0].status.as_deref(), Some("awaiting_permission"));
        assert_eq!(rows[0].updated_at.as_deref(), Some("2026-09-14T09:00:00Z"));
    }

    #[test]
    fn subagent_sessions_are_hidden_by_default_but_remain_addressable() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("child-session");
        std::fs::create_dir(&session_dir).unwrap();
        let parsed = summary(
            r#"{"session_id":"child-session","session_kind":"subagent","generated_title":"child"}"#,
        );

        let row = to_session_summary(&session_dir, parsed, "/tmp".into()).unwrap();
        assert!(row.hidden);
        assert_eq!(row.session_kind.as_deref(), Some("subagent"));
    }

    #[test]
    fn explicit_visibility_override_can_expose_a_subagent_session() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("visible-child");
        std::fs::create_dir(&session_dir).unwrap();
        let parsed =
            summary(r#"{"session_id":"visible-child","session_kind":"subagent","hidden":false}"#);

        assert!(
            !to_session_summary(&session_dir, parsed, "/tmp".into())
                .unwrap()
                .hidden
        );
    }

    #[test]
    fn scoped_catalog_keeps_hidden_subagents_addressable() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_root = temp.path().join("sessions");
        let cwd_dir = temp.path().join("workspace");
        std::fs::create_dir_all(&cwd_dir).unwrap();
        let cwd = cwd_dir
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let session_dir = sessions_root
            .join(echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd))
            .join("child-session");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            serde_json::json!({
                "session_id": "child-session",
                "cwd": cwd,
                "session_kind": "subagent",
            })
            .to_string(),
        )
        .unwrap();

        let rows = list_sessions_from_root(&sessions_root, &cwd, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "child-session");
        assert!(rows[0].hidden);
        assert!(persisted_session_exists_from_root(&sessions_root, "child-session").unwrap());
        assert!(!persisted_session_exists_from_root(&sessions_root, "missing").unwrap());
    }

    #[test]
    fn reads_upstream_session_summary_field() {
        let parsed = summary(r#"{"session_summary":"First prompt"}"#);
        assert_eq!(display_title(&parsed).as_deref(), Some("First prompt"));
    }

    #[test]
    fn historical_auto_title_drops_reasoning_and_keeps_final_title() {
        let parsed = summary(
            r#"{
                "session_summary":"<think>private reasoning</think>Professional title",
                "generated_title":"<think>private reasoning</think>Professional title",
                "title_is_manual":false
            }"#,
        );
        assert_eq!(
            display_title(&parsed).as_deref(),
            Some("Professional title")
        );
    }

    #[test]
    fn historical_unclosed_reasoning_title_is_hidden() {
        let parsed = summary(
            r#"{
                "session_summary":"<think>The user only said hello twice",
                "generated_title":"<think>The user only said hello twice"
            }"#,
        );
        assert_eq!(display_title(&parsed), None);
    }

    #[test]
    fn historical_expert_marker_title_recovers_from_display_text() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("expert-session");
        std::fs::create_dir(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("updates.jsonl"),
            serde_json::json!({
                "method": "session/update",
                "params": {
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "content": {
                            "type": "text",
                            "text": "<!--EXPERT_PERSONA_BEGIN-->\nexpert\n<!--EXPERT_PERSONA_END-->\n\n帮我写一个关于秋天的文章",
                            "_meta": { "displayText": "帮我写一个关于秋天的文章" }
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            recover_title_from_updates(&session_dir).as_deref(),
            Some("帮我写一个关于秋天的文章")
        );
        let parsed = summary(
            r#"{
                "session_id":"expert-session",
                "session_summary":"<!--EXPERT_PERSONA_BEGIN-->",
                "generated_title":"<!--EXPERT_PERSONA_BEGIN-->"
            }"#,
        );
        assert_eq!(
            to_session_summary(&session_dir, parsed, "/tmp".into())
                .unwrap()
                .title,
            "帮我写一个关于秋天的文章"
        );
    }

    #[test]
    fn historical_expert_marker_title_recovers_from_legacy_raw_text() {
        let temp = tempfile::tempdir().unwrap();
        let update = serde_json::json!({
            "params": {
                "update": {
                    "sessionUpdate": "user_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "<!--EXPERT_PERSONA_BEGIN-->\nexpert\n<!--EXPERT_PERSONA_END-->\n\n修复登录跳转问题"
                    }
                }
            }
        });
        std::fs::write(
            temp.path().join("updates.jsonl"),
            format!("not-json\n{}\n", update),
        )
        .unwrap();
        assert_eq!(
            recover_title_from_updates(temp.path()).as_deref(),
            Some("修复登录跳转问题")
        );
    }

    #[test]
    fn manual_title_is_not_interpreted_as_model_reasoning() {
        let parsed = summary(
            r#"{
                "session_summary":"fallback",
                "generated_title":"Document <think> XML tag",
                "title_is_manual":true
            }"#,
        );
        assert_eq!(
            display_title(&parsed).as_deref(),
            Some("Document <think> XML tag")
        );
    }

    #[test]
    fn nested_info_cwd_is_preserved_for_workspace_recovery() {
        let parsed = summary(r#"{"info":{"cwd":"/tmp/nested"}}"#);
        assert_eq!(summary_cwd(&parsed), "/tmp/nested");
    }

    #[test]
    fn missing_summary_cwd_only_matches_its_authoritative_encoded_directory() {
        let requested = "/tmp/project-a";
        let matching = std::path::PathBuf::from(
            echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(requested),
        );
        let other = std::path::PathBuf::from(
            echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname("/tmp/project-b"),
        );
        assert!(workspace_dir_matches_request(&matching, requested, None));
        assert!(!workspace_dir_matches_request(&other, requested, None));
    }

    #[test]
    fn summary_cannot_claim_another_session_directory_id() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("authoritative-id");
        let matching = summary(r#"{"session_id":"authoritative-id"}"#);
        assert_eq!(
            authoritative_session_id(&session_dir, &matching).as_deref(),
            Some("authoritative-id")
        );

        let forged = summary(r#"{"session_id":"different-session"}"#);
        assert!(authoritative_session_id(&session_dir, &forged).is_none());
        let nested = summary(r#"{"info":{"id":"different-session"}}"#);
        assert!(authoritative_session_id(&session_dir, &nested).is_none());
    }

    #[test]
    fn persisted_titles_are_bounded_before_reaching_the_renderer() {
        let long = "x".repeat(MAX_TITLE_CHARS + 100);
        let parsed = summary(&format!(
            r#"{{"generated_title":"{long}","title_is_manual":true}}"#
        ));
        assert_eq!(
            display_title(&parsed).unwrap().chars().count(),
            MAX_TITLE_CHARS
        );
    }

    #[test]
    fn oversized_summary_is_rejected_before_json_allocation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("summary.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_SUMMARY_BYTES + 1).unwrap();
        assert!(read_summary_file(&path).is_none());
    }

    #[test]
    fn global_catalog_recovers_sessions_across_default_cwd_changes() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_root = temp.path().join("sessions");
        let legacy_cwd = temp.path().join("legacy-home");
        let current_cwd = temp.path().join("Documents").join("EchoAgent");
        std::fs::create_dir_all(&legacy_cwd).unwrap();
        std::fs::create_dir_all(&current_cwd).unwrap();

        for (cwd, id, title) in [
            (&legacy_cwd, "legacy-session", "旧任务"),
            (&current_cwd, "current-session", "新任务"),
        ] {
            let cwd = cwd.canonicalize().unwrap().to_string_lossy().into_owned();
            let session_dir = sessions_root
                .join(echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd))
                .join(id);
            std::fs::create_dir_all(&session_dir).unwrap();
            std::fs::write(
                session_dir.join("summary.json"),
                serde_json::json!({
                    "session_id": id,
                    "cwd": cwd.clone(),
                    "generated_title": title,
                    "title_is_manual": true,
                })
                .to_string(),
            )
            .unwrap();
        }

        let legacy_cwd_string = legacy_cwd
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let no_cwd_dir = sessions_root
            .join(echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&legacy_cwd_string))
            .join("legacy-without-cwd");
        std::fs::create_dir_all(&no_cwd_dir).unwrap();
        std::fs::write(
            no_cwd_dir.join("summary.json"),
            serde_json::json!({
                "session_id": "legacy-without-cwd",
                "generated_title": "更早的任务",
                "title_is_manual": true,
            })
            .to_string(),
        )
        .unwrap();

        let rows = list_all_sessions_from_root(&sessions_root).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| row.session_id == "legacy-session"
            && row.cwd == legacy_cwd.canonicalize().unwrap().to_string_lossy()));
        assert!(rows.iter().any(|row| row.session_id == "current-session"
            && row.cwd == current_cwd.canonicalize().unwrap().to_string_lossy()));
        assert!(rows
            .iter()
            .any(|row| row.session_id == "legacy-without-cwd" && row.cwd == legacy_cwd_string));
    }

    #[test]
    fn automatic_task_directories_stay_in_catalog_but_not_recent_folders() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_root = temp.path().join("sessions");
        let base = temp.path().canonicalize().unwrap();
        let automatic = crate::session_workspace::prepare_task_directory(
            &base,
            &uuid::Uuid::now_v7().to_string(),
        )
        .unwrap();
        let manual = base.join("repository");
        std::fs::create_dir(&manual).unwrap();
        for (id, cwd) in [("automatic-task", automatic), ("manual-task", manual)] {
            let cwd = cwd.to_string_lossy().into_owned();
            let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd);
            let record = sessions_root.join(encoded).join(id);
            std::fs::create_dir_all(&record).unwrap();
            std::fs::write(
                record.join("summary.json"),
                serde_json::json!({"session_id":id,"cwd":cwd,"generated_title":id}).to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            list_all_sessions_from_root(&sessions_root).unwrap().len(),
            2
        );
        let recent = super::list_workspaces_from_root_with_default(&sessions_root, Some(&base));
        assert_eq!(recent.len(), 1);
        assert!(recent[0].cwd.ends_with("repository"));
    }

    #[test]
    fn workspace_counts_exclude_hidden_subagent_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_root = temp.path().join("sessions");
        let cwd = temp.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap().to_string_lossy().into_owned();
        let encoded = echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd);
        for (id, title, session_kind) in [
            ("parent", "主任务", None),
            ("child", "子代理", Some("subagent")),
        ] {
            let session_dir = sessions_root.join(&encoded).join(id);
            std::fs::create_dir_all(&session_dir).unwrap();
            std::fs::write(
                session_dir.join("summary.json"),
                serde_json::json!({
                    "session_id": id,
                    "cwd": cwd.clone(),
                    "generated_title": title,
                    "session_kind": session_kind,
                })
                .to_string(),
            )
            .unwrap();
        }

        let rows = list_workspaces_from_root(&sessions_root);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_count, 1);
        assert_eq!(rows[0].last_title.as_deref(), Some("主任务"));
    }

    #[cfg(unix)]
    #[test]
    fn summary_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.json");
        let link = temp.path().join("summary.json");
        std::fs::write(&target, r#"{"session_id":"secret"}"#).unwrap();
        symlink(target, &link).unwrap();
        assert!(read_summary_file(&link).is_none());
    }
}
