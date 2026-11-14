//! EchoAgent's own metadata sidecar.
//!
//! EchoAgent's `summary.json` (and the in-memory `Summary` it serializes) does NOT
//! support a `pinned` field — it only knows its own schema, and writing an
//! unknown key would be clobbered the next time EchoAgent flushes. So we keep
//! EchoAgent-only state (pinned, archived, expert, lifecycle and per-session
//! permission bindings) in a separate file:
//! `~/.echo-agent/echoagent-state.json`.
//!
//! Read on every `list_sessions` call and merged into the per-session
//! `SessionSummary`. The shape is intentionally a small versioned object so
//! we can extend it later (starred, hidden, custom tags, …) without a
//! migration.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_SESSION_ENTRIES: usize = 10_000;
const MAX_SESSION_ID_CHARS: usize = 256;
const MAX_EXPERT_ID_CHARS: usize = 256;
const MAX_EXPERT_NAME_CHARS: usize = 512;
const MAX_EXPERT_SOURCE_CHARS: usize = 64;
const MAX_EXPERT_AVATAR_CHARS: usize = 4_096;
const MAX_STATUS_TIMESTAMP_CHARS: usize = 128;
const PERMISSION_MODES: [&str; 3] = ["ask", "auto", "always-approve"];
const AGENT_MODES: [&str; 5] = ["default", "ask", "plan", "browser_use", "computer_use"];
const SESSION_STATUSES: [&str; 12] = [
    "working",
    "completed",
    "failed",
    "pending",
    "planning",
    "awaiting_permission",
    "awaiting_answer",
    "awaiting_approval",
    "pausing",
    "paused",
    "stopping",
    "stopped",
];
const TRANSIENT_SESSION_STATUSES: [&str; 7] = [
    "working",
    "planning",
    "awaiting_permission",
    "awaiting_answer",
    "awaiting_approval",
    "pausing",
    "stopping",
];

static STATE_TRANSACTION: OnceLock<Mutex<()>> = OnceLock::new();

fn state_transaction() -> &'static Mutex<()> {
    STATE_TRANSACTION.get_or_init(|| Mutex::new(()))
}

/// Expert binding for a session — records which expert was summoned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertBinding {
    /// Expert id (local agent name; legacy ids remain readable).
    pub expert_id: String,
    /// Display name shown in the UI badge.
    pub expert_name: String,
    /// "local" for current bindings; historical source values remain readable.
    pub source: String,
    /// Local avatar image path (for the topbar/composer badge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_local: Option<String>,
}

/// Last renderer-observed lifecycle state for a session. EchoAgent's upstream
/// summary has no turn status, so this is kept beside our other session-only
/// metadata. `updated_at` lets catalog consumers sort by the real state change
/// rather than the session's original project-link timestamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionLifecycle {
    pub status: String,
    pub updated_at: String,
}

/// Durable provenance, independent of the parent's later edits or deletion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionContinuation {
    pub parent_session_id: String,
    pub parent_title: String,
    pub prompt_index: u32,
    pub prompt_id: Option<String>,
    pub inherited_turn_count: u32,
    pub temporary_title: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoAgentState {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Session ids the user pinned to the top of the sidebar.
    #[serde(default)]
    pub pinned_sessions: Vec<String>,
    /// Session ids the user archived (hidden from the sidebar).
    #[serde(default)]
    pub archived_sessions: Vec<String>,
    /// Expert bindings: session_id → ExpertBinding.
    #[serde(default)]
    pub expert_sessions: HashMap<String, ExpertBinding>,
    /// Permission mode owned by each task/session. Missing entries fail closed
    /// to `ask`; there is intentionally no user-selectable global fallback.
    #[serde(default)]
    pub session_permission_modes: HashMap<String, String>,
    /// Active product mode for each task. Kept outside Runtime history so
    /// Browser Use / Computer Use survives app and renderer upgrades.
    #[serde(default)]
    pub session_agent_modes: HashMap<String, String>,
    /// Immutable canonical directory recorded when a session is first created/loaded.
    #[serde(default)]
    pub session_workspaces: HashMap<String, String>,
    /// User task description, never an execution token. Restoring it stays paused.
    #[serde(default)]
    pub session_desktop_tasks: HashMap<String, String>,
    /// Last lifecycle state: session_id → SessionLifecycle.
    #[serde(default)]
    pub session_statuses: HashMap<String, SessionLifecycle>,
    #[serde(default)]
    pub session_continuations: HashMap<String, SessionContinuation>,
}

impl Default for EchoAgentState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            pinned_sessions: Vec::new(),
            archived_sessions: Vec::new(),
            expert_sessions: HashMap::new(),
            session_permission_modes: HashMap::new(),
            session_agent_modes: HashMap::new(),
            session_workspaces: HashMap::new(),
            session_desktop_tasks: HashMap::new(),
            session_statuses: HashMap::new(),
            session_continuations: HashMap::new(),
        }
    }
}

impl EchoAgentState {
    /// Snapshot of pinned ids as a `HashSet` for O(1) membership checks when
    /// merging into the session list.
    pub fn pinned_set(&self) -> HashSet<String> {
        self.pinned_sessions.iter().cloned().collect()
    }

    /// Snapshot of archived ids as a `HashSet` for O(1) membership checks when
    /// filtering the session list.
    pub fn archived_set(&self) -> HashSet<String> {
        self.archived_sessions.iter().cloned().collect()
    }

    /// Expert bindings map for merging into the session list.
    pub fn expert_map(&self) -> &HashMap<String, ExpertBinding> {
        &self.expert_sessions
    }

    pub fn permission_mode_map(&self) -> &HashMap<String, String> {
        &self.session_permission_modes
    }

    pub fn session_status_map(&self) -> &HashMap<String, SessionLifecycle> {
        &self.session_statuses
    }
}

fn state_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-state.json")
}

fn valid_field(value: &str, max_chars: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.trim().is_empty())
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

fn valid_session_id(value: &str) -> bool {
    valid_field(value, MAX_SESSION_ID_CHARS, false)
}

fn validate_binding(binding: &ExpertBinding) -> Result<(), String> {
    if !valid_field(&binding.expert_id, MAX_EXPERT_ID_CHARS, false) {
        return Err("专家 ID 无效或过长".into());
    }
    if !valid_field(&binding.expert_name, MAX_EXPERT_NAME_CHARS, false) {
        return Err("专家名称无效或过长".into());
    }
    if !valid_field(&binding.source, MAX_EXPERT_SOURCE_CHARS, false) {
        return Err("专家来源无效或过长".into());
    }
    if binding
        .avatar_local
        .as_deref()
        .is_some_and(|value| !valid_field(value, MAX_EXPERT_AVATAR_CHARS, false))
    {
        return Err("专家头像路径无效或过长".into());
    }
    Ok(())
}

fn valid_session_status(value: &str) -> bool {
    SESSION_STATUSES.contains(&value)
}

fn validate_lifecycle(lifecycle: &SessionLifecycle) -> Result<(), String> {
    if !valid_session_status(&lifecycle.status) {
        return Err("会话状态无效".into());
    }
    if !valid_field(&lifecycle.updated_at, MAX_STATUS_TIMESTAMP_CHARS, false)
        || chrono::DateTime::parse_from_rfc3339(&lifecycle.updated_at).is_err()
    {
        return Err("会话状态时间无效".into());
    }
    Ok(())
}

fn validate_state(state: &EchoAgentState) -> Result<(), String> {
    if state.version != STATE_VERSION {
        return Err(format!(
            "unsupported EchoAgent state version: {}",
            state.version
        ));
    }
    if state.pinned_sessions.len() > MAX_SESSION_ENTRIES
        || state.archived_sessions.len() > MAX_SESSION_ENTRIES
        || state.expert_sessions.len() > MAX_SESSION_ENTRIES
        || state.session_permission_modes.len() > MAX_SESSION_ENTRIES
        || state.session_agent_modes.len() > MAX_SESSION_ENTRIES
        || state.session_workspaces.len() > MAX_SESSION_ENTRIES
        || state.session_desktop_tasks.len() > MAX_SESSION_ENTRIES
        || state.session_statuses.len() > MAX_SESSION_ENTRIES
        || state.session_continuations.len() > MAX_SESSION_ENTRIES
    {
        return Err("会话元数据条目超过安全上限".into());
    }
    if state
        .pinned_sessions
        .iter()
        .chain(&state.archived_sessions)
        .any(|value| !valid_session_id(value))
    {
        return Err("会话元数据包含无效会话 ID".into());
    }
    for (session_id, binding) in &state.expert_sessions {
        if !valid_session_id(session_id) {
            return Err("专家绑定包含无效会话 ID".into());
        }
        validate_binding(binding)?;
    }
    for (session_id, mode) in &state.session_permission_modes {
        if !valid_session_id(session_id) || !PERMISSION_MODES.contains(&mode.as_str()) {
            return Err("会话权限模式包含无效数据".into());
        }
    }
    for (session_id, mode) in &state.session_agent_modes {
        if !valid_session_id(session_id) || !AGENT_MODES.contains(&mode.as_str()) {
            return Err("会话代理模式包含无效数据".into());
        }
    }
    for (id, cwd) in &state.session_workspaces {
        if !valid_session_id(id) || !valid_field(cwd, 16384, false) {
            return Err("会话目录绑定无效".into());
        }
    }
    for (id, scope) in &state.session_desktop_tasks {
        if !valid_session_id(id) || scope.is_empty() || scope.len() > 16384 {
            return Err("电脑任务范围无效".into());
        }
    }
    for (session_id, lifecycle) in &state.session_statuses {
        if !valid_session_id(session_id) {
            return Err("会话状态包含无效会话 ID".into());
        }
        validate_lifecycle(lifecycle)?;
    }
    for (id, origin) in &state.session_continuations {
        validate_continuation(id, origin)?;
    }
    Ok(())
}

fn validate_continuation(id: &str, origin: &SessionContinuation) -> Result<(), String> {
    if !valid_session_id(id)
        || !valid_session_id(&origin.parent_session_id)
        || id == origin.parent_session_id
        || !valid_field(&origin.parent_title, 512, false)
        || !valid_field(&origin.temporary_title, 512, false)
        || origin
            .prompt_id
            .as_deref()
            .is_some_and(|id| !valid_session_id(id))
        || origin.inherited_turn_count == 0
        || chrono::DateTime::parse_from_rfc3339(&origin.created_at).is_err()
    {
        return Err("接续对话来源格式无效".into());
    }
    Ok(())
}

pub fn save_continuation(
    id: &str,
    origin: SessionContinuation,
    source: &str,
    cwd: &Path,
) -> Result<(), String> {
    validate_continuation(id, &origin)?;
    update_state_at(&state_path(), |state| {
        if state.session_continuations.contains_key(id) {
            return Ok(((), false));
        }
        state
            .session_continuations
            .insert(id.into(), origin.clone());
        if let Some(binding) = state.expert_sessions.get(source).cloned() {
            state.expert_sessions.insert(id.into(), binding);
        }
        if let Some(mode) = state.session_agent_modes.get(source).cloned() {
            state.session_agent_modes.insert(id.into(), mode);
        }
        let mode = state
            .session_permission_modes
            .get(source)
            .cloned()
            .unwrap_or_else(|| "ask".into());
        state.session_permission_modes.insert(id.into(), mode);
        state
            .session_workspaces
            .insert(id.into(), cwd.to_string_lossy().into_owned());
        Ok(((), true))
    })
}

fn sanitize_state(mut state: EchoAgentState) -> EchoAgentState {
    if state.version != STATE_VERSION {
        return EchoAgentState::default();
    }
    let mut pinned_seen = HashSet::new();
    state.pinned_sessions.retain(|value| {
        valid_session_id(value)
            && pinned_seen.insert(value.clone())
            && pinned_seen.len() <= MAX_SESSION_ENTRIES
    });
    let mut archived_seen = HashSet::new();
    state.archived_sessions.retain(|value| {
        valid_session_id(value)
            && archived_seen.insert(value.clone())
            && archived_seen.len() <= MAX_SESSION_ENTRIES
    });
    state.expert_sessions.retain(|session_id, binding| {
        valid_session_id(session_id) && validate_binding(binding).is_ok()
    });
    if state.expert_sessions.len() > MAX_SESSION_ENTRIES {
        let mut keys = state.expert_sessions.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys.into_iter().skip(MAX_SESSION_ENTRIES) {
            state.expert_sessions.remove(&key);
        }
    }
    state.session_permission_modes.retain(|session_id, mode| {
        valid_session_id(session_id) && PERMISSION_MODES.contains(&mode.as_str())
    });
    if state.session_permission_modes.len() > MAX_SESSION_ENTRIES {
        let mut keys = state
            .session_permission_modes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys.into_iter().skip(MAX_SESSION_ENTRIES) {
            state.session_permission_modes.remove(&key);
        }
    }
    state.session_agent_modes.retain(|session_id, mode| {
        valid_session_id(session_id) && AGENT_MODES.contains(&mode.as_str())
    });
    if state.session_agent_modes.len() > MAX_SESSION_ENTRIES {
        let mut keys = state
            .session_agent_modes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys.into_iter().skip(MAX_SESSION_ENTRIES) {
            state.session_agent_modes.remove(&key);
        }
    }
    state
        .session_continuations
        .retain(|id, origin| validate_continuation(id, origin).is_ok());
    let mut keys = state
        .session_continuations
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    for key in keys.into_iter().skip(MAX_SESSION_ENTRIES) {
        state.session_continuations.remove(&key);
    }
    state
        .session_desktop_tasks
        .retain(|id, scope| valid_session_id(id) && !scope.is_empty() && scope.len() <= 16384);
    state.session_statuses.retain(|session_id, lifecycle| {
        valid_session_id(session_id) && validate_lifecycle(lifecycle).is_ok()
    });
    if state.session_statuses.len() > MAX_SESSION_ENTRIES {
        let mut keys = state.session_statuses.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys.into_iter().skip(MAX_SESSION_ENTRIES) {
            state.session_statuses.remove(&key);
        }
    }
    state
}

fn read_state_from(path: &Path, strict: bool) -> Result<EchoAgentState, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(EchoAgentState::default())
        }
        Err(error) => return Err(format!("inspect state {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "EchoAgent state must be a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(format!(
            "EchoAgent state exceeds {} MiB",
            MAX_STATE_BYTES / 1024 / 1024
        ));
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(path, MAX_STATE_BYTES)
        .map_err(|error| format!("read state {}: {error}", path.display()))?;
    let state: EchoAgentState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse state {}: {error}", path.display()))?;
    if strict {
        validate_state(&state)?;
        Ok(state)
    } else {
        Ok(sanitize_state(state))
    }
}

fn with_state_file_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _process_guard = state_transaction()
        .lock()
        .map_err(|_| "EchoAgent state transaction lock is poisoned".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("state path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create state dir {}: {error}", parent.display()))?;
    crate::paths::harden_private_dir(parent)?;
    let lock_path = parent.join(".echoagent-state.lock");
    if std::fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "state lock must not be a symlink: {}",
            lock_path.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock_file = options
        .open(&lock_path)
        .map_err(|error| format!("open state lock {}: {error}", lock_path.display()))?;
    crate::paths::harden_private_file(&lock_path)?;
    let mut acquired = false;
    for _ in 0..200 {
        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(format!("lock state {}: {error}", lock_path.display())),
        }
    }
    if !acquired {
        return Err(format!(
            "timed out waiting for EchoAgent state lock {}",
            lock_path.display()
        ));
    }
    let result = operation();
    let unlock = FileExt::unlock(&lock_file)
        .map_err(|error| format!("unlock state {}: {error}", lock_path.display()));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn write_state_to(path: &Path, state: &EchoAgentState) -> Result<(), String> {
    validate_state(state)?;
    let body = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("serialize EchoAgent state: {error}"))?;
    if body.len() as u64 > MAX_STATE_BYTES {
        return Err(format!(
            "EchoAgent state exceeds {} MiB",
            MAX_STATE_BYTES / 1024 / 1024
        ));
    }
    crate::paths::write_private_file(path, &body)
}

fn update_state_at<T>(
    path: &Path,
    operation: impl FnOnce(&mut EchoAgentState) -> Result<(T, bool), String>,
) -> Result<T, String> {
    with_state_file_lock(path, || {
        // Mutations are deliberately strict: malformed, oversized or future
        // state is never silently replaced with defaults.
        let mut state = read_state_from(path, true)?;
        let (result, changed) = operation(&mut state)?;
        if changed {
            write_state_to(path, &state)?;
        }
        Ok(result)
    })
}

/// Read EchoAgent state. Missing/corrupt → default (we never block startup on
/// sidecar state; a corrupt file is left in place rather than rewritten so the
/// user can recover it manually if needed).
pub fn read_state() -> EchoAgentState {
    let path = state_path();
    with_state_file_lock(&path, || read_state_from(&path, false)).unwrap_or_default()
}

fn set_pinned_at(path: &Path, session_id: &str, pinned: bool) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    update_state_at(path, |state| {
        let already = state
            .pinned_sessions
            .iter()
            .any(|value| value == session_id);
        if pinned && !already {
            if state.pinned_sessions.len() >= MAX_SESSION_ENTRIES {
                return Err("置顶会话数量超过安全上限".into());
            }
            state.pinned_sessions.push(session_id.to_string());
            Ok((pinned, true))
        } else if !pinned && already {
            state.pinned_sessions.retain(|value| value != session_id);
            Ok((pinned, true))
        } else {
            Ok((pinned, false))
        }
    })
}

/// Set the pinned flag for one session. Returns the new pinned state so the
/// command layer can echo it back to the frontend.
pub fn set_pinned(session_id: &str, pinned: bool) -> Result<bool, String> {
    set_pinned_at(&state_path(), session_id, pinned)
}

fn set_archived_at(path: &Path, session_id: &str, archived: bool) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    update_state_at(path, |state| {
        let already = state
            .archived_sessions
            .iter()
            .any(|value| value == session_id);
        if archived && !already {
            if state.archived_sessions.len() >= MAX_SESSION_ENTRIES {
                return Err("归档会话数量超过安全上限".into());
            }
            state.archived_sessions.push(session_id.to_string());
            Ok((archived, true))
        } else if !archived && already {
            state.archived_sessions.retain(|value| value != session_id);
            Ok((archived, true))
        } else {
            Ok((archived, false))
        }
    })
}

/// Set the archived flag for one session. Returns the new archived state so
/// the command layer can echo it back to the frontend.
pub fn set_archived(session_id: &str, archived: bool) -> Result<bool, String> {
    set_archived_at(&state_path(), session_id, archived)
}

/// Bind an expert to a session. Overwrites any previous binding for the same
/// session id. Returns `true` on success.
pub fn set_expert(session_id: &str, binding: ExpertBinding) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    validate_binding(&binding)?;
    update_state_at(&state_path(), |state| {
        if !state.expert_sessions.contains_key(session_id)
            && state.expert_sessions.len() >= MAX_SESSION_ENTRIES
        {
            return Err("专家绑定数量超过安全上限".into());
        }
        state
            .expert_sessions
            .insert(session_id.to_string(), binding);
        Ok((true, true))
    })
}

/// Remove the expert binding for a session. Returns `true` if a binding was
/// removed, `false` if there was none.
pub fn clear_expert(session_id: &str) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    update_state_at(&state_path(), |state| {
        let removed = state.expert_sessions.remove(session_id).is_some();
        Ok((removed, removed))
    })
}

/// Return the task-owned permission mode. Legacy sessions deliberately fall
/// back to Ask instead of inheriting a former global Always/Auto preference.
pub fn permission_mode(session_id: &str) -> String {
    if !valid_session_id(session_id) {
        return "ask".into();
    }
    read_state()
        .session_permission_modes
        .get(session_id)
        .cloned()
        .unwrap_or_else(|| "ask".into())
}

fn set_permission_mode_at(path: &Path, session_id: &str, mode: &str) -> Result<String, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if !PERMISSION_MODES.contains(&mode) {
        return Err(format!("unknown permission mode: {mode}"));
    }
    update_state_at(path, |state| {
        if !state.session_permission_modes.contains_key(session_id)
            && state.session_permission_modes.len() >= MAX_SESSION_ENTRIES
        {
            return Err("会话权限模式数量超过安全上限".into());
        }
        let changed = state
            .session_permission_modes
            .get(session_id)
            .is_none_or(|current| current != mode);
        state
            .session_permission_modes
            .insert(session_id.to_string(), mode.to_string());
        Ok((mode.to_string(), changed))
    })
}

pub fn set_permission_mode(session_id: &str, mode: &str) -> Result<String, String> {
    set_permission_mode_at(&state_path(), session_id, mode)
}

/// Return the task-owned agent mode. Legacy sessions default to the regular
/// agent and never acquire automation capabilities implicitly.
pub fn agent_mode(session_id: &str) -> Option<String> {
    if !valid_session_id(session_id) {
        return None;
    }
    read_state().session_agent_modes.get(session_id).cloned()
}

fn set_agent_mode_at(path: &Path, session_id: &str, mode: &str) -> Result<String, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if !AGENT_MODES.contains(&mode) {
        return Err(format!("unknown agent mode: {mode}"));
    }
    update_state_at(path, |state| {
        if !state.session_agent_modes.contains_key(session_id)
            && state.session_agent_modes.len() >= MAX_SESSION_ENTRIES
        {
            return Err("会话代理模式数量超过安全上限".into());
        }
        let changed = state
            .session_agent_modes
            .get(session_id)
            .is_none_or(|current| current != mode);
        state
            .session_agent_modes
            .insert(session_id.to_string(), mode.to_string());
        Ok((mode.to_string(), changed))
    })
}

pub fn set_agent_mode(session_id: &str, mode: &str) -> Result<String, String> {
    set_agent_mode_at(&state_path(), session_id, mode)
}

/// Persist a binding once; reopening, project moves and Runtime restarts cannot retarget it.
fn bind_session_workspace_at(path: &Path, session_id: &str, cwd: &str) -> Result<(), String> {
    if !valid_session_id(session_id) || !valid_field(cwd, 16384, false) {
        return Err("会话目录绑定无效".into());
    }
    update_state_at(path, |state| {
        if let Some(original) = state.session_workspaces.get(session_id) {
            if original != cwd {
                return Err("本会话的工作目录已固定；如需使用其他目录，请新建任务".into());
            }
            return Ok(((), false));
        }
        if state.session_workspaces.len() >= MAX_SESSION_ENTRIES {
            return Err("会话目录绑定数量超过上限".into());
        }
        state
            .session_workspaces
            .insert(session_id.into(), cwd.into());
        Ok(((), true))
    })
}

pub fn bind_session_workspace(session_id: &str, cwd: &Path) -> Result<(), String> {
    bind_session_workspace_at(&state_path(), session_id, &cwd.to_string_lossy())
}

pub fn session_workspace(session_id: &str) -> Option<PathBuf> {
    read_state()
        .session_workspaces
        .get(session_id)
        .map(PathBuf::from)
}

pub fn desktop_task_scope(session_id: &str) -> Option<String> {
    read_state().session_desktop_tasks.get(session_id).cloned()
}
/// Save context without replacing the main agent's manually selected profile.
/// Execution grants remain process-local and are never restored from this text.
pub fn set_desktop_task_scope(session_id: &str, scope: &str) -> Result<(), String> {
    set_desktop_task_scope_at(&state_path(), session_id, scope)
}
fn set_desktop_task_scope_at(path: &Path, session_id: &str, scope: &str) -> Result<(), String> {
    if !valid_session_id(session_id) || scope.trim().is_empty() || scope.len() > 16384 {
        return Err("任务说明无效或超过 16 KiB".into());
    }
    update_state_at(path, |state| {
        if state.session_desktop_tasks.len() >= MAX_SESSION_ENTRIES
            && !state.session_desktop_tasks.contains_key(session_id)
        {
            return Err("电脑任务数量超过上限".into());
        }
        let changed = state
            .session_desktop_tasks
            .get(session_id)
            .is_none_or(|v| v != scope);
        state
            .session_desktop_tasks
            .insert(session_id.into(), scope.into());
        Ok(((), changed))
    })
}

#[cfg(test)]
fn clear_permission_mode_at(path: &Path, session_id: &str) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    update_state_at(path, |state| {
        let removed = state.session_permission_modes.remove(session_id).is_some();
        Ok((removed, removed))
    })
}

fn set_session_status_at(
    path: &Path,
    session_id: &str,
    status: &str,
    updated_at: &str,
) -> Result<(), String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    let lifecycle = SessionLifecycle {
        status: status.to_string(),
        updated_at: updated_at.to_string(),
    };
    validate_lifecycle(&lifecycle)?;
    update_state_at(path, |state| {
        if !state.session_statuses.contains_key(session_id)
            && state.session_statuses.len() >= MAX_SESSION_ENTRIES
        {
            return Err("会话状态数量超过安全上限".into());
        }
        let current_is_newer = state
            .session_statuses
            .get(session_id)
            .and_then(|current| chrono::DateTime::parse_from_rfc3339(&current.updated_at).ok())
            .zip(chrono::DateTime::parse_from_rfc3339(updated_at).ok())
            .is_some_and(|(current, incoming)| current > incoming);
        if current_is_newer {
            return Ok(((), false));
        }
        let changed = state.session_statuses.get(session_id) != Some(&lifecycle);
        state
            .session_statuses
            .insert(session_id.to_string(), lifecycle);
        Ok(((), changed))
    })
}

/// Persist a session lifecycle transition, rejecting out-of-order observations.
pub fn set_session_status(session_id: &str, status: &str, updated_at: &str) -> Result<(), String> {
    set_session_status_at(&state_path(), session_id, status, updated_at)
}

fn mark_transient_statuses_stopped_at(path: &Path, updated_at: &str) -> Result<usize, String> {
    if !valid_field(updated_at, MAX_STATUS_TIMESTAMP_CHARS, false)
        || chrono::DateTime::parse_from_rfc3339(updated_at).is_err()
    {
        return Err("会话状态时间无效".into());
    }
    update_state_at(path, |state| {
        let mut changed = 0;
        for lifecycle in state.session_statuses.values_mut() {
            if TRANSIENT_SESSION_STATUSES.contains(&lifecycle.status.as_str()) {
                lifecycle.status = "stopped".into();
                lifecycle.updated_at = updated_at.to_string();
                changed += 1;
            }
        }
        Ok((changed, changed > 0))
    })
}

/// A newly spawned Runtime cannot still own work from the previous process.
/// Convert only volatile states; terminal, pending and deliberately paused
/// sessions remain intact. Renderer-only reloads reuse the Runtime and skip it.
pub fn mark_transient_statuses_stopped() -> Result<usize, String> {
    mark_transient_statuses_stopped_at(&state_path(), &chrono::Utc::now().to_rfc3339())
}

fn clear_session_metadata_at(path: &Path, session_id: &str) -> Result<bool, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    update_state_at(path, |state| {
        let mut changed = false;
        let pinned_len = state.pinned_sessions.len();
        state.pinned_sessions.retain(|value| value != session_id);
        changed |= state.pinned_sessions.len() != pinned_len;
        let archived_len = state.archived_sessions.len();
        state.archived_sessions.retain(|value| value != session_id);
        changed |= state.archived_sessions.len() != archived_len;
        changed |= state.expert_sessions.remove(session_id).is_some();
        changed |= state.session_permission_modes.remove(session_id).is_some();
        changed |= state.session_agent_modes.remove(session_id).is_some();
        changed |= state.session_workspaces.remove(session_id).is_some();
        changed |= state.session_desktop_tasks.remove(session_id).is_some();
        changed |= state.session_statuses.remove(session_id).is_some();
        changed |= state.session_continuations.remove(session_id).is_some();
        Ok((changed, changed))
    })
}

/// Remove every sidecar entry owned by a deleted session.
pub fn clear_session_metadata(session_id: &str) -> Result<bool, String> {
    clear_session_metadata_at(&state_path(), session_id)
}

// ---------- unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deleting_parent_keeps_child_provenance_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let origin = SessionContinuation {
            parent_session_id: "parent".into(),
            parent_title: "原对话".into(),
            prompt_index: 1,
            prompt_id: Some("p1".into()),
            inherited_turn_count: 2,
            temporary_title: "原对话 · 新方向".into(),
            created_at: "2026-11-03T00:00:00Z".into(),
        };
        update_state_at(&path, |state| {
            state
                .session_continuations
                .insert("child".into(), origin.clone());
            Ok(((), true))
        })
        .unwrap();
        clear_session_metadata_at(&path, "parent").unwrap();
        let restored = read_state_from(&path, true).unwrap();
        assert_eq!(restored.session_continuations["child"], origin);
        clear_session_metadata_at(&path, "child").unwrap();
        assert!(read_state_from(&path, true)
            .unwrap()
            .session_continuations
            .is_empty());
    }

    #[test]
    fn workspace_binding_survives_reload_and_rejects_retargeting() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        bind_session_workspace_at(&state_path, "session-a", "/original").unwrap();
        bind_session_workspace_at(&state_path, "session-a", "/original").unwrap();
        assert!(bind_session_workspace_at(&state_path, "session-a", "/other").is_err());
        assert_eq!(
            read_state_from(&state_path, true)
                .unwrap()
                .session_workspaces["session-a"],
            "/original"
        );
        clear_session_metadata_at(&state_path, "session-a").unwrap();
        assert!(read_state_from(&state_path, true)
            .unwrap()
            .session_workspaces
            .is_empty());
    }

    // --- EchoAgentState::default ---

    #[test]
    fn default_state() {
        let state = EchoAgentState::default();
        assert_eq!(state.version, 1);
        assert!(state.pinned_sessions.is_empty());
        assert!(state.archived_sessions.is_empty());
        assert!(state.session_permission_modes.is_empty());
        assert!(state.session_agent_modes.is_empty());
        assert!(state.session_statuses.is_empty());
    }

    // --- pinned_set / archived_set ---

    #[test]
    fn pinned_set_converts_to_hashset() {
        let state = EchoAgentState {
            version: 1,
            pinned_sessions: vec!["s1".into(), "s2".into(), "s1".into()],
            archived_sessions: vec![],
            expert_sessions: HashMap::new(),
            session_permission_modes: HashMap::new(),
            session_agent_modes: HashMap::new(),
            session_workspaces: HashMap::new(),
            session_desktop_tasks: HashMap::new(),
            session_statuses: HashMap::new(),
            session_continuations: HashMap::new(),
        };
        let set = state.pinned_set();
        assert_eq!(set.len(), 2); // deduplicated
        assert!(set.contains("s1"));
        assert!(set.contains("s2"));
    }

    #[test]
    fn archived_set_converts_to_hashset() {
        let state = EchoAgentState {
            version: 1,
            pinned_sessions: vec![],
            archived_sessions: vec!["a1".into(), "a2".into()],
            expert_sessions: HashMap::new(),
            session_permission_modes: HashMap::new(),
            session_agent_modes: HashMap::new(),
            session_workspaces: HashMap::new(),
            session_desktop_tasks: HashMap::new(),
            session_statuses: HashMap::new(),
            session_continuations: HashMap::new(),
        };
        let set = state.archived_set();
        assert_eq!(set.len(), 2);
        assert!(set.contains("a1"));
        assert!(set.contains("a2"));
    }

    #[test]
    fn empty_sets() {
        let state = EchoAgentState::default();
        assert!(state.pinned_set().is_empty());
        assert!(state.archived_set().is_empty());
    }

    // --- serde round-trip ---

    #[test]
    fn state_serde_roundtrip() {
        let state = EchoAgentState {
            version: 1,
            pinned_sessions: vec!["s1".into()],
            archived_sessions: vec!["a1".into(), "a2".into()],
            expert_sessions: HashMap::new(),
            session_permission_modes: HashMap::from([("s1".into(), "auto".into())]),
            session_agent_modes: HashMap::from([("s1".into(), "browser_use".into())]),
            session_workspaces: HashMap::new(),
            session_desktop_tasks: HashMap::new(),
            session_statuses: HashMap::from([(
                "s1".into(),
                SessionLifecycle {
                    status: "completed".into(),
                    updated_at: "2026-09-14T08:00:00Z".into(),
                },
            )]),
            session_continuations: HashMap::new(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: EchoAgentState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.pinned_sessions, vec!["s1"]);
        assert_eq!(parsed.archived_sessions, vec!["a1", "a2"]);
        assert_eq!(
            parsed
                .session_permission_modes
                .get("s1")
                .map(String::as_str),
            Some("auto")
        );
    }

    #[test]
    fn state_deserialize_with_missing_fields() {
        // Old format might not have all fields
        let json = r#"{"version":1}"#;
        let state: EchoAgentState = serde_json::from_str(json).unwrap();
        assert!(state.pinned_sessions.is_empty());
        assert!(state.archived_sessions.is_empty());
        assert!(state.session_permission_modes.is_empty());
        assert!(state.session_agent_modes.is_empty());
        assert!(state.session_statuses.is_empty());
    }

    #[test]
    fn permission_modes_are_persisted_and_cleared_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");

        assert_eq!(
            set_permission_mode_at(&path, "session-a", "auto").unwrap(),
            "auto"
        );
        assert_eq!(
            set_permission_mode_at(&path, "session-b", "always-approve").unwrap(),
            "always-approve"
        );

        let state = read_state_from(&path, true).unwrap();
        assert_eq!(
            state
                .session_permission_modes
                .get("session-a")
                .map(String::as_str),
            Some("auto")
        );
        assert_eq!(
            state
                .session_permission_modes
                .get("session-b")
                .map(String::as_str),
            Some("always-approve")
        );

        assert!(clear_permission_mode_at(&path, "session-a").unwrap());
        let state = read_state_from(&path, true).unwrap();
        assert!(!state.session_permission_modes.contains_key("session-a"));
        assert_eq!(
            state
                .session_permission_modes
                .get("session-b")
                .map(String::as_str),
            Some("always-approve")
        );
    }

    #[test]
    fn permission_mode_metadata_rejects_invalid_values_without_mutating_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");

        set_permission_mode_at(&path, "session-a", "ask").unwrap();
        assert!(set_permission_mode_at(&path, "session-b", "unsafe").is_err());
        assert!(set_permission_mode_at(&path, "\n", "auto").is_err());

        let state = read_state_from(&path, true).unwrap();
        assert_eq!(state.session_permission_modes.len(), 1);
        assert_eq!(
            state
                .session_permission_modes
                .get("session-a")
                .map(String::as_str),
            Some("ask")
        );
    }

    #[test]
    fn agent_modes_are_persisted_and_reject_unknown_values() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");

        assert_eq!(
            set_agent_mode_at(&path, "session-a", "browser_use").unwrap(),
            "browser_use"
        );
        assert!(set_agent_mode_at(&path, "session-b", "unsafe").is_err());
        let state = read_state_from(&path, true).unwrap();
        assert_eq!(
            state
                .session_agent_modes
                .get("session-a")
                .map(String::as_str),
            Some("browser_use")
        );
        assert!(!state.session_agent_modes.contains_key("session-b"));
    }

    #[test]
    fn desktop_context_preserves_main_agent_mode_and_never_persists_authority() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");
        set_agent_mode_at(&path, "session-a", "default").unwrap();
        let scope = "请在 TextEdit 保存文稿\n\n[用户补充]\n保存到 /tmp/report.txt";
        set_desktop_task_scope_at(&path, "session-a", scope).unwrap();
        let state = read_state_from(&path, true).unwrap();
        assert_eq!(state.session_agent_modes["session-a"], "default");
        assert_eq!(state.session_desktop_tasks["session-a"], scope);
        let committed = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&committed).contains("authorized"));
        assert!(set_desktop_task_scope_at(&path, "session-a", &"x".repeat(16385)).is_err());
        assert!(set_desktop_task_scope_at(&path, "bad\n", scope).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), committed);
    }

    #[test]
    fn session_statuses_are_validated_persisted_and_recovered_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");

        set_session_status_at(
            &path,
            "session-running",
            "awaiting_answer",
            "2026-09-14T08:00:00Z",
        )
        .unwrap();
        set_session_status_at(&path, "session-done", "completed", "2026-09-14T08:01:00Z").unwrap();
        // Async IPC calls may finish out of order. An earlier observation must
        // never overwrite a newer terminal state.
        set_session_status_at(&path, "session-done", "working", "2026-09-14T08:00:30Z").unwrap();
        assert!(
            set_session_status_at(&path, "session-invalid", "made_up", "2026-09-14T08:02:00Z",)
                .is_err()
        );

        assert_eq!(
            mark_transient_statuses_stopped_at(&path, "2026-09-14T09:00:00Z").unwrap(),
            1
        );
        let state = read_state_from(&path, true).unwrap();
        assert_eq!(state.session_statuses["session-running"].status, "stopped");
        assert_eq!(
            state.session_statuses["session-running"].updated_at,
            "2026-09-14T09:00:00Z"
        );
        assert_eq!(state.session_statuses["session-done"].status, "completed");
    }

    #[test]
    fn deleting_session_clears_all_sidecar_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");
        let mut state = EchoAgentState::default();
        state.pinned_sessions.push("session-a".into());
        state.archived_sessions.push("session-a".into());
        state
            .session_permission_modes
            .insert("session-a".into(), "ask".into());
        state
            .session_agent_modes
            .insert("session-a".into(), "computer_use".into());
        state.session_statuses.insert(
            "session-a".into(),
            SessionLifecycle {
                status: "failed".into(),
                updated_at: "2026-09-14T08:00:00Z".into(),
            },
        );
        write_state_to(&path, &state).unwrap();

        assert!(clear_session_metadata_at(&path, "session-a").unwrap());
        let cleared = read_state_from(&path, true).unwrap();
        assert!(cleared.pinned_sessions.is_empty());
        assert!(cleared.archived_sessions.is_empty());
        assert!(cleared.session_permission_modes.is_empty());
        assert!(cleared.session_agent_modes.is_empty());
        assert!(cleared.session_statuses.is_empty());
    }

    #[test]
    fn set_pinned_and_archived_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("echoagent-state.json");

        // --- pinned lifecycle ---
        // Pin
        let result = set_pinned_at(&path, "session-1", true).unwrap();
        assert!(result);
        let state = read_state_from(&path, false).unwrap();
        assert!(state.pinned_sessions.contains(&"session-1".to_string()));

        // Pin again (idempotent — no duplicate)
        let result = set_pinned_at(&path, "session-1", true).unwrap();
        assert!(result);
        let state = read_state_from(&path, false).unwrap();
        assert_eq!(
            state
                .pinned_sessions
                .iter()
                .filter(|s| *s == "session-1")
                .count(),
            1
        );

        // Unpin
        let result = set_pinned_at(&path, "session-1", false).unwrap();
        assert!(!result);
        let state = read_state_from(&path, false).unwrap();
        assert!(!state.pinned_sessions.contains(&"session-1".to_string()));

        // Unpin again (idempotent)
        let result = set_pinned_at(&path, "session-1", false).unwrap();
        assert!(!result);

        // --- archived lifecycle ---
        // Archive
        let result = set_archived_at(&path, "session-2", true).unwrap();
        assert!(result);
        let state = read_state_from(&path, false).unwrap();
        assert!(state.archived_sessions.contains(&"session-2".to_string()));

        // Archive again (idempotent)
        let result = set_archived_at(&path, "session-2", true).unwrap();
        assert!(result);
        let state = read_state_from(&path, false).unwrap();
        assert_eq!(
            state
                .archived_sessions
                .iter()
                .filter(|s| *s == "session-2")
                .count(),
            1
        );

        // Unarchive
        let result = set_archived_at(&path, "session-2", false).unwrap();
        assert!(!result);
        let state = read_state_from(&path, false).unwrap();
        assert!(!state.archived_sessions.contains(&"session-2".to_string()));

        // A malformed state remains recoverable and must never be replaced by
        // a mutation that silently starts from defaults.
        let corrupt = b"{not valid json";
        std::fs::write(&path, corrupt).unwrap();
        assert!(set_pinned_at(&path, "session-3", true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        // Oversized files are rejected before allocation and invalid inputs
        // never enter the persisted collections.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_STATE_BYTES + 1).unwrap();
        assert!(read_state_from(&path, true).is_err());
        assert!(set_pinned_at(&path, &"x".repeat(MAX_SESSION_ID_CHARS + 1), true).is_err());
    }

    #[test]
    fn state_validation_bounds_expert_fields_and_collection_sizes() {
        let mut state = EchoAgentState::default();
        state.expert_sessions.insert(
            "session".into(),
            ExpertBinding {
                expert_id: "id".into(),
                expert_name: "x".repeat(MAX_EXPERT_NAME_CHARS + 1),
                source: "local".into(),
                avatar_local: None,
            },
        );
        assert!(validate_state(&state).is_err());

        let too_many = EchoAgentState {
            pinned_sessions: (0..=MAX_SESSION_ENTRIES)
                .map(|index| format!("session-{index}"))
                .collect(),
            ..EchoAgentState::default()
        };
        assert!(validate_state(&too_many).is_err());
    }
}
