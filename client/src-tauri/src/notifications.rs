//! EchoAgent notification log — a local inbox for EchoAgent events.
//!
//! EchoAgent's "智能体邮箱" (agent mailbox) is a Tencent email integration
//! (send/receive mail, turn emails into tasks). EchoAgent has no email backend,
//! so EchoAgent uses this tab as a local inbox for requests requiring action,
//! connection problems, and task results. Routine state changes stay in their
//! own views instead of filling the inbox.
//!
//! Storage: `~/.echo-agent/echoagent-notifications.json` (capped at 200 entries;
//! older entries drop off FIFO).

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use crate::commands::AppState;

/// Notification kind. Mirrors the EchoAgent event channels we already subscribe to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Permission,
    #[serde(alias = "folderTrust", alias = "folder-trust")]
    FolderTrust,
    #[serde(alias = "taskUpdate", alias = "task-update")]
    TaskUpdate,
    #[serde(alias = "planMode", alias = "plan-mode")]
    PlanMode,
    #[serde(alias = "mcpStatus", alias = "mcp-status")]
    McpStatus,
    #[serde(alias = "modelsUpdate", alias = "models-update")]
    ModelsUpdate,
    Summary,
    #[serde(
        alias = "sessionComplete",
        alias = "session-complete",
        alias = "complete"
    )]
    SessionComplete,
    Error,
    Info,
}

impl NotificationKind {
    fn from_str(s: &str) -> Self {
        match s {
            "permission" => Self::Permission,
            "folder_trust" | "folder-trust" => Self::FolderTrust,
            "task_update" | "task-update" => Self::TaskUpdate,
            "plan_mode" | "plan-mode" => Self::PlanMode,
            "mcp_status" | "mcp-status" => Self::McpStatus,
            "models_update" | "models-update" => Self::ModelsUpdate,
            "summary" => Self::Summary,
            "session_complete" | "complete" => Self::SessionComplete,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEntry {
    /// Monotonic id (timestamp-based).
    pub id: u64,
    /// Stable producer identity for crash-safe local append-once (old JSON omits it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    pub kind: NotificationKind,
    /// ISO timestamp.
    pub at: String,
    /// Human-readable title.
    pub title: String,
    /// Optional detail/body (raw event JSON or short text).
    #[serde(default)]
    pub body: Option<String>,
    /// Optional related session id (for permission/summary/etc.).
    #[serde(default)]
    pub session_id: Option<String>,
    /// Exact pending permission, when this entry represents an approval request.
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    /// Severity: "info" | "warn" | "error".
    #[serde(default = "default_severity")]
    pub severity: String,
    /// Whether the user has dismissed/read this entry.
    #[serde(default)]
    pub read: bool,
}

fn default_severity() -> String {
    "info".into()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NotificationStore {
    #[serde(default)]
    pub entries: Vec<NotificationEntry>,
}

const MAX_ENTRIES: usize = 200;
const MAX_NOTIFICATION_STORE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHANNEL_STORE_BYTES: u64 = 256 * 1024;
const MAX_CHANNELS: usize = 32;
const MAX_CHANNEL_ID_CHARS: usize = 128;
const MAX_CHANNEL_LABEL_CHARS: usize = 128;
const MAX_ENDPOINT_CHARS: usize = 2_048;
const MAX_TITLE_CHARS: usize = 256;
const MAX_BODY_CHARS: usize = 16_384;
const MAX_SESSION_ID_CHARS: usize = 256;
const MAX_TITLE_BYTES: usize = 512;
const MAX_BODY_BYTES: usize = 3_584;
const MAX_SESSION_ID_BYTES: usize = 512;
const MAX_REQUEST_ID_CHARS: usize = 256;
const MAX_TIMESTAMP_CHARS: usize = 64;
const STORE_LOCK_ATTEMPTS: usize = 200;
const STORE_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
static LOG_ACCESS: OnceLock<Mutex<()>> = OnceLock::new();

fn log_access() -> &'static Mutex<()> {
    LOG_ACCESS.get_or_init(|| Mutex::new(()))
}

fn store_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-notifications.json")
}

fn lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn acquire_file_lock_at(path: &Path, label: &str) -> Result<std::fs::File, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!("{label}目录必须是真实目录，不能是符号链接"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建{label}目录失败：{error}"))?;
            let metadata = std::fs::symlink_metadata(parent)
                .map_err(|error| format!("检查{label}目录失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("{label}目录必须是真实目录，不能是符号链接"));
            }
        }
        Err(error) => return Err(format!("检查{label}目录失败：{error}")),
    }
    crate::paths::harden_private_dir(parent)?;

    let lock_path = lock_path(path);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!("{label}锁必须是普通文件，不能是符号链接"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("检查{label}锁失败：{error}")),
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(&lock_path)
        .map_err(|error| format!("打开{label}锁失败：{error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("读取{label}锁失败：{error}"))?
        .is_file()
    {
        return Err(format!("{label}锁在打开期间被替换"));
    }
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("收紧{label}锁权限失败：{error}"))?;
    for _ in 0..STORE_LOCK_ATTEMPTS {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(STORE_LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(format!("锁定{label}失败：{error}")),
        }
    }
    Err(format!(
        "等待其他 EchoAgent 实例保存{label}超时，请稍后重试"
    ))
}

fn with_file_access_at<T>(
    path: &Path,
    process_lock: &Mutex<()>,
    label: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _process_guard = process_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _file_guard = acquire_file_lock_at(path, label)?;
    operation()
}

fn read_bounded(path: &Path, limit: u64, label: &str) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("检查{label}失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label}必须是普通文件，不能是符号链接"));
    }
    if metadata.len() > limit {
        return Err(format!("{label}超过大小限制（{} KiB）", limit / 1024));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(path)
        .map_err(|error| format!("打开{label}失败：{error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("读取{label}元数据失败：{error}"))?
        .is_file()
    {
        return Err(format!("{label}在打开期间被替换"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取{label}失败：{error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{label}超过大小限制（{} KiB）", limit / 1024));
    }
    Ok(Some(bytes))
}

fn truncate_text(value: &str, max_chars: usize, max_bytes: usize, preserve_layout: bool) -> String {
    let mut output = String::new();
    let mut count = 0;
    for character in value.chars() {
        if character.is_control() && !(preserve_layout && (character == '\n' || character == '\t'))
        {
            continue;
        }
        if count >= max_chars || output.len() + character.len_utf8() > max_bytes {
            break;
        }
        output.push(character);
        count += 1;
    }
    output
}

fn normalize_severity(value: &str) -> &'static str {
    match value {
        "warn" => "warn",
        "error" => "error",
        _ => "info",
    }
}

fn normalize_title(value: &str) -> String {
    let title = truncate_text(value.trim(), MAX_TITLE_CHARS, MAX_TITLE_BYTES, false);
    if title.is_empty() {
        "EchoAgent 通知".into()
    } else {
        title
    }
}

fn valid_severity(value: &str) -> bool {
    matches!(value, "info" | "warn" | "error")
}

fn validate_entry(entry: &NotificationEntry) -> Result<(), String> {
    if entry.at.chars().count() > MAX_TIMESTAMP_CHARS
        || entry.at.chars().any(char::is_control)
        || chrono::DateTime::parse_from_rfc3339(&entry.at).is_err()
        || entry.title.trim().is_empty()
        || entry.title.chars().count() > MAX_TITLE_CHARS
        || entry.title.len() > MAX_TITLE_BYTES
        || entry.title.chars().any(char::is_control)
        || entry.body.as_deref().is_some_and(|body| {
            body.chars().count() > MAX_BODY_CHARS
                || body.len() > MAX_BODY_BYTES
                || body.chars().any(|character| {
                    character.is_control() && character != '\n' && character != '\t'
                })
        })
        || entry.session_id.as_deref().is_some_and(|id| {
            id.chars().count() > MAX_SESSION_ID_CHARS
                || id.len() > MAX_SESSION_ID_BYTES
                || id.chars().any(char::is_control)
        })
        || entry.request_id.as_deref().is_some_and(|id| {
            id.is_empty()
                || id.chars().count() > MAX_REQUEST_ID_CHARS
                || id.chars().any(char::is_control)
        })
        || entry.automation_id.as_deref().is_some_and(|id| {
            id.is_empty()
                || id.chars().count() > MAX_REQUEST_ID_CHARS
                || id.chars().any(char::is_control)
        })
        || entry
            .dedupe_key
            .as_deref()
            .is_some_and(|key| key.is_empty() || key.len() > 512)
        || !valid_severity(&entry.severity)
    {
        return Err("通知记录包含超限或非法字段".into());
    }
    Ok(())
}

fn validate_notification_store(store: &NotificationStore) -> Result<(), String> {
    if store.entries.len() > MAX_ENTRIES {
        return Err(format!("通知记录数量超过 {MAX_ENTRIES} 条限制"));
    }
    let mut ids = std::collections::HashSet::with_capacity(store.entries.len());
    for entry in &store.entries {
        validate_entry(entry)?;
        if !ids.insert(entry.id) {
            return Err("通知记录包含重复 id".into());
        }
    }
    Ok(())
}

/// Read the notification log. A missing file is an empty inbox; malformed or
/// oversized data is reported so a subsequent mutation cannot erase it.
fn read_store_at(path: &Path) -> Result<NotificationStore, String> {
    let Some(bytes) = read_bounded(path, MAX_NOTIFICATION_STORE_BYTES, "通知记录")? else {
        return Ok(NotificationStore::default());
    };
    let store: NotificationStore =
        serde_json::from_slice(&bytes).map_err(|error| format!("解析通知记录失败：{error}"))?;
    validate_notification_store(&store)?;
    Ok(store)
}

pub fn read_store() -> Result<NotificationStore, String> {
    let path = store_path();
    with_file_access_at(&path, log_access(), "通知记录", || read_store_at(&path))
}

/// Atomic write.
fn write_store_at(path: &Path, store: &NotificationStore) -> Result<(), String> {
    validate_notification_store(store)?;
    let body =
        serde_json::to_vec_pretty(store).map_err(|error| format!("序列化通知记录失败：{error}"))?;
    if body.len() as u64 > MAX_NOTIFICATION_STORE_BYTES {
        return Err(format!(
            "通知记录超过大小限制（{} KiB），未写入磁盘",
            MAX_NOTIFICATION_STORE_BYTES / 1024
        ));
    }
    crate::paths::write_private_file(path, &body)
}

fn update_store_at<T>(
    path: &Path,
    update: impl FnOnce(&mut NotificationStore) -> Result<T, String>,
) -> Result<T, String> {
    with_file_access_at(path, log_access(), "通知记录", || {
        let mut store = read_store_at(path)?;
        let result = update(&mut store)?;
        write_store_at(path, &store)?;
        Ok(result)
    })
}

/// Append a notification. Called by the frontend (via command) when it
/// receives a EchoAgent event it wants logged. Caps the log at MAX_ENTRIES.
pub fn append(
    kind: NotificationKind,
    title: &str,
    body: Option<&str>,
    session_id: Option<&str>,
    severity: &str,
) -> Result<(), String> {
    append_with_target(kind, title, body, session_id, None, None, severity).map(|_| ())
}

pub fn append_with_target(
    kind: NotificationKind,
    title: &str,
    body: Option<&str>,
    session_id: Option<&str>,
    request_id: Option<&str>,
    automation_id: Option<&str>,
    severity: &str,
) -> Result<u64, String> {
    append_once_at(
        &store_path(),
        kind,
        title,
        body,
        session_id,
        request_id,
        automation_id,
        severity,
        None,
    )
}

/// Automation record identity is distinct from permission/request navigation.
pub(crate) fn append_automation_once(
    record_id: &str,
    kind: NotificationKind,
    title: &str,
    body: Option<&str>,
    session_id: Option<&str>,
    automation_id: &str,
    severity: &str,
) -> Result<u64, String> {
    let key = format!("automation-terminal:{record_id}");
    append_once_at(
        &store_path(),
        kind,
        title,
        body,
        session_id,
        None,
        Some(automation_id),
        severity,
        Some(&key),
    )
}

#[allow(clippy::too_many_arguments)]
fn append_once_at(
    path: &Path,
    kind: NotificationKind,
    title: &str,
    body: Option<&str>,
    session_id: Option<&str>,
    request_id: Option<&str>,
    automation_id: Option<&str>,
    severity: &str,
    dedupe_key: Option<&str>,
) -> Result<u64, String> {
    if dedupe_key.is_some_and(|key| key.is_empty() || key.len() > 512) {
        return Err("通知幂等标识无效".into());
    }
    update_store_at(path, |store| {
        if let Some(key) = dedupe_key {
            if let Some(entry) = store
                .entries
                .iter()
                .find(|entry| entry.dedupe_key.as_deref() == Some(key))
            {
                return Ok(entry.id);
            }
        }
        let id = match store.entries.iter().map(|entry| entry.id).max() {
            Some(u64::MAX) => return Err("通知记录 id 已耗尽，请清空通知后重试".into()),
            Some(last_id) => last_id + 1,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(1),
        };
        store.entries.push(NotificationEntry {
            id,
            dedupe_key: dedupe_key.map(str::to_string),
            kind,
            at: chrono::Local::now().to_rfc3339(),
            title: normalize_title(title),
            body: body.map(|value| truncate_text(value, MAX_BODY_CHARS, MAX_BODY_BYTES, true)),
            session_id: session_id.and_then(|value| {
                let value = truncate_text(
                    value.trim(),
                    MAX_SESSION_ID_CHARS,
                    MAX_SESSION_ID_BYTES,
                    false,
                );
                (!value.is_empty()).then_some(value)
            }),
            request_id: request_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            automation_id: automation_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            severity: normalize_severity(severity).to_string(),
            read: false,
        });
        if store.entries.len() > MAX_ENTRIES {
            let excess = store.entries.len() - MAX_ENTRIES;
            store.entries.drain(0..excess);
        }
        Ok(id)
    })
}
// ---------- Tauri commands ----------

/// Append a notification (frontend calls this when it receives a EchoAgent event).
#[tauri::command]
pub fn notification_append(
    _state: State<'_, AppState>,
    kind: String,
    title: String,
    body: Option<String>,
    session_id: Option<String>,
    severity: Option<String>,
) -> Result<(), String> {
    append(
        NotificationKind::from_str(&kind),
        &title,
        body.as_deref(),
        session_id.as_deref(),
        severity.as_deref().unwrap_or("info"),
    )
}

/// List notifications (newest first).
#[tauri::command]
pub fn notification_list(_state: State<'_, AppState>) -> Result<Vec<NotificationEntry>, String> {
    let mut entries = read_store()?.entries;
    entries.reverse();
    Ok(entries)
}

/// Mark a notification as read.
#[tauri::command]
pub fn notification_mark_read(_state: State<'_, AppState>, id: u64) -> Result<(), String> {
    let path = store_path();
    update_store_at(&path, |store| {
        let entry = store
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("通知 {id} 不存在"))?;
        entry.read = true;
        Ok(())
    })
}

/// Mark all as read.
#[tauri::command]
pub fn notification_mark_all_read(_state: State<'_, AppState>) -> Result<(), String> {
    let path = store_path();
    update_store_at(&path, |store| {
        for entry in &mut store.entries {
            entry.read = true;
        }
        Ok(())
    })
}

/// Clear all notifications.
#[tauri::command]
pub fn notification_clear(_state: State<'_, AppState>) -> Result<(), String> {
    let path = store_path();
    with_file_access_at(&path, log_access(), "通知记录", || {
        write_store_at(&path, &NotificationStore::default())
    })
}

// ---------- external notification channels ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    SlackWebhook,
    DiscordWebhook,
    GenericWebhook,
    Email,
    Desktop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyChannel {
    pub id: String,
    pub label: String,
    pub kind: ChannelKind,
    #[serde(default)]
    pub endpoint: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyMessage {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default = "default_severity")]
    pub level: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub notification_id: Option<u64>,
    /// Generic title used on the desktop when task titles are hidden.
    #[serde(default)]
    pub hidden_title: Option<String>,
    /// Routine results only need an OS banner while the app is in the background.
    #[serde(default)]
    pub background_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationOpen {
    pub id: String,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub automation_id: Option<String>,
}

static PENDING_OPENS: OnceLock<Mutex<VecDeque<NotificationOpen>>> = OnceLock::new();
static VISIBLE_SESSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn pending_opens() -> &'static Mutex<VecDeque<NotificationOpen>> {
    PENDING_OPENS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn visible_session() -> &'static Mutex<Option<String>> {
    VISIBLE_SESSION.get_or_init(|| Mutex::new(None))
}

#[tauri::command]
pub fn notification_set_visible_session(session_id: Option<String>) -> Result<(), String> {
    if session_id.as_deref().is_some_and(|id| {
        id.is_empty()
            || id.chars().count() > MAX_SESSION_ID_CHARS
            || id.chars().any(char::is_control)
    }) {
        return Err("无效的会话标识".into());
    }
    *visible_session()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = session_id;
    Ok(())
}

#[tauri::command]
pub fn notification_take_pending_opens() -> Vec<NotificationOpen> {
    let mut pending = pending_opens()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.drain(..).collect()
}

fn record_notification_open(app: &AppHandle, target: NotificationOpen) {
    let mut pending = pending_opens()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if pending.len() >= 128 {
        pending.pop_front();
    }
    pending.push_back(target);
    drop(pending);
    crate::show_main_window(app);
    let _ = app.emit("notification://opened", ());
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryResult {
    pub id: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ChannelStore {
    #[serde(default)]
    channels: Vec<NotifyChannel>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopNotifyStatus {
    enabled: bool,
    source: &'static str,
    permission: &'static str,
    policy_error: Option<String>,
}

fn desktop_status(store: &ChannelStore, policy_error: Option<String>) -> DesktopNotifyStatus {
    let configured = store
        .channels
        .iter()
        .any(|channel| channel.kind == ChannelKind::Desktop);
    DesktopNotifyStatus {
        enabled: policy_error.is_none()
            && (!configured
                || store
                    .channels
                    .iter()
                    .any(|channel| channel.kind == ChannelKind::Desktop && channel.enabled)),
        source: if policy_error.is_some() {
            "policy"
        } else if configured {
            "configured"
        } else {
            "default"
        },
        // Cross-platform OS permission/banner visibility cannot be queried reliably.
        permission: "unknown",
        policy_error,
    }
}

#[tauri::command]
pub fn notify_desktop_status() -> Result<DesktopNotifyStatus, String> {
    Ok(desktop_status(
        &read_channels()?,
        crate::policy::require_feature("notifications").err(),
    ))
}

fn set_desktop_enabled(store: &mut ChannelStore, enabled: bool) -> Result<(), String> {
    if store
        .channels
        .iter()
        .any(|channel| channel.kind == ChannelKind::Desktop)
    {
        let mut enabled_once = false;
        for channel in &mut store.channels {
            if channel.kind == ChannelKind::Desktop {
                // Legacy configurations may contain several desktop records.
                // Preserve them, while delivering a single desktop banner.
                channel.enabled = enabled && !enabled_once;
                enabled_once |= channel.enabled;
            }
        }
        Ok(())
    } else {
        upsert_channel_in_store(
            store,
            NotifyChannel {
                id: format!("desktop-{}", uuid::Uuid::now_v7()),
                label: "桌面提醒".into(),
                kind: ChannelKind::Desktop,
                endpoint: None,
                enabled,
            },
        )
    }
}

#[tauri::command]
pub fn notify_desktop_set_enabled(enabled: bool) -> Result<(), String> {
    crate::policy::require_feature("notifications")?;
    update_channels_at(&channels_path(), |store| {
        set_desktop_enabled(store, enabled)
    })
}

static CHANNEL_ACCESS: OnceLock<Mutex<()>> = OnceLock::new();

fn channel_access() -> &'static Mutex<()> {
    CHANNEL_ACCESS.get_or_init(|| Mutex::new(()))
}

fn channels_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-notify-channels.json")
}

fn read_channels_at(path: &Path) -> Result<ChannelStore, String> {
    let Some(raw) = read_bounded(path, MAX_CHANNEL_STORE_BYTES, "通知渠道配置")? else {
        return Ok(ChannelStore::default());
    };
    let mut store: ChannelStore =
        serde_json::from_slice(&raw).map_err(|error| format!("解析通知渠道配置失败：{error}"))?;
    // Opening a mailto draft is not delivery. Old builds exposed that as an
    // email channel, so keep legacy entries visible/removable but never run
    // them from background agent events.
    for channel in &mut store.channels {
        if channel.kind == ChannelKind::Email {
            channel.enabled = false;
        }
    }
    validate_channel_store(&store)?;
    Ok(store)
}

fn read_channels() -> Result<ChannelStore, String> {
    let path = channels_path();
    with_file_access_at(&path, channel_access(), "通知渠道配置", || {
        read_channels_at(&path)
    })
}

fn write_channels_at(path: &Path, store: &ChannelStore) -> Result<(), String> {
    validate_channel_store(store)?;
    let raw = serde_json::to_vec_pretty(store).map_err(|e| format!("serialize channels: {e}"))?;
    if raw.len() as u64 > MAX_CHANNEL_STORE_BYTES {
        return Err(format!(
            "通知渠道配置超过大小限制（{} KiB）",
            MAX_CHANNEL_STORE_BYTES / 1024
        ));
    }
    crate::paths::write_private_file(path, &raw)
}

fn update_channels_at<T>(
    path: &Path,
    update: impl FnOnce(&mut ChannelStore) -> Result<T, String>,
) -> Result<T, String> {
    with_file_access_at(path, channel_access(), "通知渠道配置", || {
        let mut store = read_channels_at(path)?;
        let result = update(&mut store)?;
        write_channels_at(path, &store)?;
        Ok(result)
    })
}

fn validate_channel_id(id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty()
        || id.chars().count() > MAX_CHANNEL_ID_CHARS
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-.:".contains(character))
    {
        return Err("渠道 id 过长或包含非法字符".into());
    }
    Ok(())
}

fn validate_channel_store(store: &ChannelStore) -> Result<(), String> {
    if store.channels.len() > MAX_CHANNELS {
        return Err(format!("通知渠道数量不能超过 {MAX_CHANNELS} 个"));
    }
    let mut ids = std::collections::HashSet::with_capacity(store.channels.len());
    for channel in &store.channels {
        validate_channel_shape(channel)?;
        if channel.kind == ChannelKind::Email && channel.enabled {
            return Err("邮件自动投递尚未配置，不能启用".into());
        }
        if !ids.insert(channel.id.as_str()) {
            return Err(format!("通知渠道 id {} 重复", channel.id));
        }
    }
    Ok(())
}

fn validate_channel_shape(channel: &NotifyChannel) -> Result<(), String> {
    validate_channel_id(&channel.id)?;
    let label = channel.label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_CHANNEL_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err("渠道显示名为空、过长或包含控制字符".into());
    }
    if channel.endpoint.as_deref().is_some_and(|endpoint| {
        endpoint.chars().count() > MAX_ENDPOINT_CHARS || endpoint.chars().any(char::is_control)
    }) {
        return Err("通知渠道 endpoint 过长或包含控制字符".into());
    }
    Ok(())
}

fn validate_channel(channel: &NotifyChannel) -> Result<(), String> {
    validate_channel_shape(channel)?;
    match channel.kind {
        ChannelKind::SlackWebhook | ChannelKind::DiscordWebhook | ChannelKind::GenericWebhook => {
            let endpoint = channel.endpoint.as_deref().unwrap_or_default().trim();
            if endpoint.chars().count() > MAX_ENDPOINT_CHARS {
                return Err("Webhook endpoint 过长".into());
            }
            let url = reqwest::Url::parse(endpoint)
                .map_err(|error| format!("Webhook endpoint URL 无效：{error}"))?;
            if !url.username().is_empty() || url.password().is_some() {
                return Err("Webhook endpoint 不得在 URL 中包含用户名或密码".into());
            }
            if url.host_str().is_none() || url.fragment().is_some() {
                return Err("Webhook endpoint 缺少主机名或包含 fragment".into());
            }
            let loopback = url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            });
            if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
                return Err(
                    "Webhook endpoint 必须使用 HTTPS（本机 localhost/回环地址除外）".into(),
                );
            }
        }
        ChannelKind::Email => return Err("邮件自动投递需要 SMTP 凭据，当前版本未开放该渠道".into()),
        ChannelKind::Desktop => {}
    }
    Ok(())
}

#[tauri::command]
pub fn notify_channels_list() -> Result<Vec<NotifyChannel>, String> {
    Ok(read_channels()?
        .channels
        .into_iter()
        .map(|mut channel| {
            if let Some(endpoint) = channel.endpoint.as_deref() {
                channel.endpoint = match channel.kind {
                    ChannelKind::Desktop => None,
                    ChannelKind::Email => endpoint
                        .split_once('@')
                        .map(|(_, domain)| format!("***@{domain}")),
                    _ => reqwest::Url::parse(endpoint).ok().map(|url| {
                        format!(
                            "{}://{}/…",
                            url.scheme(),
                            url.host_str().unwrap_or("configured")
                        )
                    }),
                };
            }
            channel
        })
        .collect())
}

#[tauri::command]
pub fn notify_channel_upsert(mut channel: NotifyChannel) -> Result<(), String> {
    crate::policy::require_feature("notifications")?;
    channel.id = channel.id.trim().to_string();
    channel.label = channel.label.trim().to_string();
    channel.endpoint = match &channel.kind {
        ChannelKind::Desktop => None,
        _ => channel
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(String::from),
    };
    let path = channels_path();
    update_channels_at(&path, |store| upsert_channel_in_store(store, channel))
}

fn upsert_channel_in_store(
    store: &mut ChannelStore,
    mut channel: NotifyChannel,
) -> Result<(), String> {
    if let Some(existing) = store.channels.iter_mut().find(|item| item.id == channel.id) {
        // The list API intentionally redacts webhook credentials. An edit
        // with no replacement URL keeps the existing secret server-side.
        if existing.kind == channel.kind && channel.endpoint.is_none() {
            channel.endpoint = existing.endpoint.clone();
        }
        validate_channel(&channel)?;
        *existing = channel;
    } else {
        validate_channel(&channel)?;
        if store.channels.len() >= MAX_CHANNELS {
            return Err(format!("通知渠道数量不能超过 {MAX_CHANNELS} 个"));
        }
        store.channels.push(channel);
    }
    Ok(())
}

#[tauri::command]
pub fn notify_channel_remove(id: String) -> Result<(), String> {
    crate::policy::require_feature("notifications")?;
    let id = id.trim();
    validate_channel_id(id)?;
    let path = channels_path();
    update_channels_at(&path, |store| remove_channel_in_store(store, id))
}

fn remove_channel_in_store(store: &mut ChannelStore, id: &str) -> Result<(), String> {
    if store
        .channels
        .iter()
        .any(|channel| channel.id == id && channel.kind == ChannelKind::Desktop)
    {
        return Err("桌面提醒请使用开启/关闭开关，保留显式关闭偏好".into());
    }
    let before = store.channels.len();
    store.channels.retain(|channel| channel.id != id);
    if store.channels.len() == before {
        return Err(format!("通知渠道 {id} 不存在"));
    }
    Ok(())
}

#[tauri::command]
pub fn notify_channel_set_enabled(id: String, enabled: bool) -> Result<(), String> {
    crate::policy::require_feature("notifications")?;
    let id = id.trim();
    validate_channel_id(id)?;
    let path = channels_path();
    update_channels_at(&path, |store| {
        let channel = store
            .channels
            .iter_mut()
            .find(|channel| channel.id == id)
            .ok_or_else(|| format!("通知渠道 {id} 不存在"))?;
        if enabled && channel.kind == ChannelKind::Email {
            return Err("邮件自动投递尚未配置，请使用 Webhook 渠道".into());
        }
        if enabled {
            validate_channel(channel)?;
        }
        channel.enabled = enabled;
        Ok(())
    })
}

fn payload(channel: &NotifyChannel, message: &NotifyMessage) -> serde_json::Value {
    match channel.kind {
        ChannelKind::SlackWebhook => {
            let title = escape_slack_mrkdwn(&message.title);
            let body = message.body.as_deref().map(escape_slack_mrkdwn);
            serde_json::json!({
                "text": format!("{} {}", level_emoji(&message.level), title),
                "blocks": [{"type":"section", "text":{"type":"mrkdwn", "text": format!("*{} {}*{}", level_emoji(&message.level), title, body.as_deref().map(|body| format!("\n{body}")).unwrap_or_default())}}]
            })
        }
        ChannelKind::DiscordWebhook => serde_json::json!({
            "content": message.title,
            "embeds": [{"title": message.title, "description": message.body, "color": if message.level == "error" { 0xdc2626 } else if message.level == "warn" { 0xd97706 } else { 0x0ea5e9 }}],
            "allowed_mentions": {"parse": []},
        }),
        _ => serde_json::json!({
            "title": message.title,
            "body": message.body,
            "level": message.level,
            "sessionId": message.session_id,
            "timestamp": chrono::Local::now().to_rfc3339(),
        }),
    }
}

fn escape_slack_mrkdwn(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn level_emoji(level: &str) -> &'static str {
    match level {
        "error" => "🔴",
        "warn" => "🟡",
        _ => "🔵",
    }
}

fn normalize_message(message: NotifyMessage) -> NotifyMessage {
    NotifyMessage {
        title: normalize_title(&message.title),
        body: message
            .body
            .as_deref()
            .map(|value| truncate_text(value, MAX_BODY_CHARS, MAX_BODY_BYTES, true)),
        level: normalize_severity(&message.level).to_string(),
        session_id: message.session_id.as_deref().and_then(|value| {
            let value = truncate_text(
                value.trim(),
                MAX_SESSION_ID_CHARS,
                MAX_SESSION_ID_BYTES,
                false,
            );
            (!value.is_empty()).then_some(value)
        }),
        request_id: message.request_id,
        automation_id: message.automation_id,
        notification_id: message.notification_id,
        hidden_title: message.hidden_title.map(|title| normalize_title(&title)),
        background_only: message.background_only,
    }
}

pub fn task_title_for_notification(session_id: &str) -> Option<String> {
    crate::sessions::list_all_sessions(true)
        .ok()?
        .into_iter()
        .find(|session| session.session_id == session_id)
        .map(|session| session.title.trim().to_string())
        .filter(|title| !title.is_empty() && title != "未命名会话" && title != "新会话")
        .map(|title| truncate_text(&title, 48, 160, false))
}

async fn show_desktop_notification(app: &AppHandle, message: &NotifyMessage) -> Result<(), String> {
    if message.background_only
        && message.session_id.as_ref().is_some_and(|session_id| {
            visible_session()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                == Some(session_id)
        })
        && app
            .get_webview_window("main")
            .is_some_and(|window| window.is_focused().unwrap_or(false))
    {
        return Ok(());
    }
    let show_title = crate::desktop_preferences::load(app)
        .map(|preferences| preferences.show_notification_task_title)
        .unwrap_or(false);
    let title = if show_title {
        message.title.clone()
    } else {
        message
            .hidden_title
            .clone()
            .unwrap_or_else(|| message.title.clone())
    };
    let body = message.body.clone().unwrap_or_default();
    if message.session_id.is_none() && message.automation_id.is_none() {
        return app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|error| format!("system notification: {error}"));
    }
    let target = NotificationOpen {
        id: message
            .notification_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        session_id: message.session_id.clone(),
        request_id: message.request_id.clone(),
        automation_id: message.automation_id.clone(),
    };
    let app = app.clone();
    // Acknowledge OS submission before reporting success. Waiting for a click
    // stays on the blocking worker and never holds up task completion.
    let (submitted, receipt) = tokio::sync::oneshot::channel();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        let _ = notify_rust::set_application(if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            &app.config().identifier
        });
        let mut notification = notify_rust::Notification::new();
        notification.summary(&title).body(&body).auto_icon();
        #[cfg(target_os = "windows")]
        if !tauri::is_dev() {
            notification.app_id(&app.config().identifier);
        }
        #[cfg(target_os = "linux")]
        notification.action("default", "打开任务");
        match notification.show() {
            Ok(handle) => {
                let _ = submitted.send(Ok(()));
                if let Err(error) = handle.wait_for_response(|response: &notify_rust::NotificationResponse| {
                    if response.is_default_action()
                        || matches!(response, notify_rust::NotificationResponse::Action(action) if action == "default")
                    {
                        record_notification_open(&app, target);
                    }
                }) {
                    tracing::warn!(%error, "failed to receive desktop notification click");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to show actionable desktop notification");
                let _ = submitted.send(Err(format!("system notification: {error}")));
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(10), receipt)
        .await
        .map_err(|_| "系统通知提交超时".to_string())?
        .map_err(|_| "系统通知投递工作进程已结束".to_string())?
}

async fn send_one(
    app: &AppHandle,
    channel: &NotifyChannel,
    message: &NotifyMessage,
) -> Result<(), String> {
    validate_channel(channel)?;
    match channel.kind {
        ChannelKind::Desktop => show_desktop_notification(app, message).await,
        ChannelKind::Email => Err("邮件自动投递未配置".into()),
        ChannelKind::SlackWebhook | ChannelKind::DiscordWebhook | ChannelKind::GenericWebhook => {
            let endpoint = channel.endpoint.as_deref().ok_or("missing endpoint")?;
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "无法初始化 Webhook HTTP 客户端".to_string())?;
            let body = payload(channel, message);
            for attempt in 0..3 {
                let (error, retryable) = match client.post(endpoint).json(&body).send().await {
                    Ok(response) if response.status().is_success() => return Ok(()),
                    Ok(response) => {
                        let status = response.status();
                        (
                            format!("Webhook 返回 HTTP {status}"),
                            status.is_server_error() || status.as_u16() == 429,
                        )
                    }
                    Err(error) => {
                        let description = if error.is_timeout() {
                            "Webhook 请求超时（结果未知，为避免重复投递未自动重试）"
                        } else if error.is_connect() {
                            "无法连接 Webhook 服务"
                        } else if error.is_request() {
                            "Webhook 请求无法发送"
                        } else {
                            "Webhook 投递失败"
                        };
                        (description.to_string(), error.is_connect())
                    }
                };
                if !retryable || attempt == 2 {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
            }
            Err("Webhook 投递失败".into())
        }
    }
}

pub async fn dispatch_external(
    app: &AppHandle,
    message: NotifyMessage,
    only_id: Option<&str>,
) -> Vec<DeliveryResult> {
    if crate::policy::require_feature("notifications").is_err() {
        return vec![DeliveryResult {
            id: only_id.unwrap_or("notifications-policy").to_string(),
            ok: false,
            error: Some("通知功能已被组织策略禁用".into()),
        }];
    }
    let message = normalize_message(message);
    let channels = read_channels().map(|store| eligible_channels(store, only_id));
    let channels = match channels {
        Ok(channels) => channels,
        Err(error) => {
            let _ = append(
                NotificationKind::Error,
                "通知渠道配置不可用",
                Some(&error),
                message.session_id.as_deref(),
                "error",
            );
            return vec![DeliveryResult {
                id: only_id.unwrap_or("channels-store").to_string(),
                ok: false,
                error: Some(error),
            }];
        }
    };
    dispatch_channels(app, channels, &message).await
}

async fn dispatch_channels(
    app: &AppHandle,
    channels: Vec<NotifyChannel>,
    message: &NotifyMessage,
) -> Vec<DeliveryResult> {
    let mut results = Vec::with_capacity(channels.len());
    for channel in channels {
        match send_one(app, &channel, message).await {
            Ok(()) => results.push(DeliveryResult {
                id: channel.id,
                ok: true,
                error: None,
            }),
            Err(error) => {
                let _ = append(
                    NotificationKind::Error,
                    "通知投递失败",
                    Some(&format!("{}：{error}", channel.label)),
                    message.session_id.as_deref(),
                    "error",
                );
                results.push(DeliveryResult {
                    id: channel.id,
                    ok: false,
                    error: Some(error),
                });
            }
        }
    }
    results
}

fn eligible_channels(store: ChannelStore, only_id: Option<&str>) -> Vec<NotifyChannel> {
    let mut desktop_selected = false;
    store
        .channels
        .into_iter()
        .filter(|channel| channel.enabled)
        .filter(|channel| only_id.map(|id| channel.id == id).unwrap_or(true))
        .filter(|channel| {
            if !matches!(channel.kind, ChannelKind::Desktop) {
                return true;
            }
            let first = !desktop_selected;
            desktop_selected = true;
            first
        })
        .collect::<Vec<_>>()
}

fn channels_with_desktop_fallback(store: ChannelStore) -> (Vec<NotifyChannel>, bool) {
    let use_builtin = !store
        .channels
        .iter()
        .any(|channel| channel.kind == ChannelKind::Desktop);
    (eligible_channels(store, None), use_builtin)
}

/// Deliver an actionable event with a built-in desktop fallback. An explicit
/// desktop channel replaces that fallback; disabling it opts out of desktop delivery.
pub async fn dispatch_with_desktop_fallback(
    app: &AppHandle,
    message: NotifyMessage,
) -> Vec<DeliveryResult> {
    if crate::policy::require_feature("notifications").is_err() {
        return vec![DeliveryResult {
            id: "notifications-policy".into(),
            ok: false,
            error: Some("通知功能已被组织策略禁用".into()),
        }];
    }
    let message = normalize_message(message);
    let (channels, use_builtin) = match read_channels().map(channels_with_desktop_fallback) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = append(
                NotificationKind::Error,
                "通知渠道配置不可用",
                Some(&error),
                message.session_id.as_deref(),
                "error",
            );
            return vec![DeliveryResult {
                id: "channels-store".into(),
                ok: false,
                error: Some(error),
            }];
        }
    };
    let mut results = dispatch_channels(app, channels, &message).await;
    if use_builtin {
        match show_desktop_notification(app, &message).await {
            Ok(()) => results.push(DeliveryResult {
                id: "builtin-desktop".into(),
                ok: true,
                error: None,
            }),
            Err(error) => {
                let error = format!("system notification: {error}");
                let _ = append(
                    NotificationKind::Error,
                    "通知投递失败",
                    Some(&error),
                    message.session_id.as_deref(),
                    "error",
                );
                results.push(DeliveryResult {
                    id: "builtin-desktop".into(),
                    ok: false,
                    error: Some(error),
                });
            }
        }
    }
    results
}

#[tauri::command]
pub async fn notify_desktop_test(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<DeliveryResult, String> {
    crate::policy::require_feature("notifications")?;
    if let Some(id) = session_id.as_deref() {
        // A native test may return to the active task, never an arbitrary target.
        state.session_workspace(id)?;
    }
    if !desktop_status(&read_channels()?, None).enabled {
        return Err("桌面提醒已关闭，请先开启再测试".into());
    }
    let message = NotifyMessage {
        title: "测试桌面提醒".into(),
        body: Some(
            if session_id.is_some() {
                "来自 EchoAgent，点击返回当前任务"
            } else {
                "来自 EchoAgent。系统设置可能影响提醒的显示"
            }
            .into(),
        ),
        session_id,
        ..Default::default()
    };
    let error = show_desktop_notification(&app, &message).await.err();
    if let Some(error) = &error {
        let _ = append(
            NotificationKind::Error,
            "通知投递失败",
            Some(error),
            message.session_id.as_deref(),
            "error",
        );
    }
    Ok(DeliveryResult {
        id: "desktop".into(),
        ok: error.is_none(),
        error,
    })
}

#[tauri::command]
pub async fn notify_channel_test(app: AppHandle, id: String) -> Result<DeliveryResult, String> {
    let results = dispatch_external(
        &app,
        NotifyMessage {
            title: "测试通知".into(),
            body: Some("来自 EchoAgent 的测试消息".into()),
            level: "info".into(),
            session_id: None,
            ..Default::default()
        },
        Some(&id),
    )
    .await;
    results
        .into_iter()
        .next()
        .ok_or_else(|| "渠道不存在、未启用或被策略禁用".into())
}

// ---------- unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_fallback_and_channels_use_one_settings_snapshot() {
        let mut store = ChannelStore::default();
        let (before, builtin_before) = channels_with_desktop_fallback(store.clone());
        set_desktop_enabled(&mut store, true).unwrap();
        let (after, builtin_after) = channels_with_desktop_fallback(store.clone());
        set_desktop_enabled(&mut store, false).unwrap();
        let (disabled, builtin_disabled) = channels_with_desktop_fallback(store);

        assert!(before.is_empty() && builtin_before);
        assert!(!builtin_after);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].kind, ChannelKind::Desktop);
        assert!(disabled.is_empty() && !builtin_disabled);
    }

    #[test]
    fn desktop_default_opt_out_survives_reload_and_cannot_be_deleted_into_fallback() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("channels.json");
        let mut store = read_channels_at(&path).unwrap();
        let initial = desktop_status(&store, None);
        assert!(initial.enabled);
        assert_eq!(initial.source, "default");
        assert_eq!(initial.permission, "unknown");
        set_desktop_enabled(&mut store, false).unwrap();
        write_channels_at(&path, &store).unwrap();
        let mut loaded = read_channels_at(&path).unwrap();
        assert!(!desktop_status(&loaded, None).enabled);
        let id = loaded.channels[0].id.clone();
        assert!(remove_channel_in_store(&mut loaded, &id).is_err());
        assert!(!desktop_status(&loaded, None).enabled);
        set_desktop_enabled(&mut loaded, true).unwrap();
        assert!(desktop_status(&loaded, None).enabled);
        assert_eq!(loaded.channels[0].id, id);
        let blocked = desktop_status(&loaded, Some("blocked by policy".into()));
        assert!(!blocked.enabled);
        assert_eq!(blocked.source, "policy");
    }

    #[test]
    fn desktop_switch_preserves_custom_channels_and_legacy_records_without_duplicate_banners() {
        let mut store = ChannelStore {
            channels: vec![
                NotifyChannel {
                    id: "one".into(),
                    label: "One".into(),
                    kind: ChannelKind::Desktop,
                    endpoint: None,
                    enabled: false,
                },
                NotifyChannel {
                    id: "two".into(),
                    label: "Two".into(),
                    kind: ChannelKind::Desktop,
                    endpoint: None,
                    enabled: true,
                },
                NotifyChannel {
                    id: "hook".into(),
                    label: "Hook".into(),
                    kind: ChannelKind::GenericWebhook,
                    endpoint: Some("https://example.test/private".into()),
                    enabled: true,
                },
            ],
        };
        store.channels[0].enabled = true;
        assert_eq!(eligible_channels(store.clone(), None).len(), 2);
        assert_eq!(eligible_channels(store.clone(), Some("two"))[0].id, "two");
        assert!(desktop_status(&store, None).enabled);
        set_desktop_enabled(&mut store, false).unwrap();
        assert!(!desktop_status(&store, None).enabled);
        set_desktop_enabled(&mut store, true).unwrap();
        assert_eq!(
            store
                .channels
                .iter()
                .filter(|channel| channel.kind == ChannelKind::Desktop && channel.enabled)
                .count(),
            1
        );
        assert_eq!(store.channels.len(), 3);
        assert_eq!(
            store.channels[2].endpoint.as_deref(),
            Some("https://example.test/private")
        );
        assert!(store.channels[2].enabled);
    }

    #[test]
    fn native_click_targets_are_drained_once_with_exact_request() {
        let mut pending = pending_opens().lock().unwrap();
        pending.clear();
        pending.push_back(NotificationOpen {
            id: "42".into(),
            session_id: Some("session-1".into()),
            request_id: Some("permission-2".into()),
            automation_id: None,
        });
        drop(pending);

        let opens = notification_take_pending_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].session_id.as_deref(), Some("session-1"));
        assert_eq!(opens[0].request_id.as_deref(), Some("permission-2"));
        assert!(notification_take_pending_opens().is_empty());
    }

    #[test]
    fn visible_session_rejects_invalid_identifiers() {
        assert!(notification_set_visible_session(Some("session-1".into())).is_ok());
        assert!(notification_set_visible_session(Some("bad\nname".into())).is_err());
        assert_eq!(
            visible_session().lock().unwrap().as_deref(),
            Some("session-1")
        );
        notification_set_visible_session(None).unwrap();
    }

    // --- NotificationKind::from_str ---

    #[test]
    fn kind_from_str_known_variants() {
        assert!(matches!(
            NotificationKind::from_str("permission"),
            NotificationKind::Permission
        ));
        assert!(matches!(
            NotificationKind::from_str("folder_trust"),
            NotificationKind::FolderTrust
        ));
        assert!(matches!(
            NotificationKind::from_str("folder-trust"),
            NotificationKind::FolderTrust
        ));
        assert!(matches!(
            NotificationKind::from_str("task_update"),
            NotificationKind::TaskUpdate
        ));
        assert!(matches!(
            NotificationKind::from_str("task-update"),
            NotificationKind::TaskUpdate
        ));
        assert!(matches!(
            NotificationKind::from_str("plan_mode"),
            NotificationKind::PlanMode
        ));
        assert!(matches!(
            NotificationKind::from_str("plan-mode"),
            NotificationKind::PlanMode
        ));
        assert!(matches!(
            NotificationKind::from_str("mcp_status"),
            NotificationKind::McpStatus
        ));
        assert!(matches!(
            NotificationKind::from_str("mcp-status"),
            NotificationKind::McpStatus
        ));
        assert!(matches!(
            NotificationKind::from_str("models_update"),
            NotificationKind::ModelsUpdate
        ));
        assert!(matches!(
            NotificationKind::from_str("models-update"),
            NotificationKind::ModelsUpdate
        ));
        assert!(matches!(
            NotificationKind::from_str("summary"),
            NotificationKind::Summary
        ));
        assert!(matches!(
            NotificationKind::from_str("session_complete"),
            NotificationKind::SessionComplete
        ));
        assert!(matches!(
            NotificationKind::from_str("complete"),
            NotificationKind::SessionComplete
        ));
        assert!(matches!(
            NotificationKind::from_str("error"),
            NotificationKind::Error
        ));
    }

    #[test]
    fn kind_from_str_unknown_falls_back_to_info() {
        assert!(matches!(
            NotificationKind::from_str("unknown"),
            NotificationKind::Info
        ));
        assert!(matches!(
            NotificationKind::from_str(""),
            NotificationKind::Info
        ));
        assert!(matches!(
            NotificationKind::from_str("PERMISSION"),
            NotificationKind::Info
        ));
    }

    // --- default_severity ---

    #[test]
    fn default_severity_is_info() {
        assert_eq!(default_severity(), "info");
    }

    // --- NotificationStore serde ---

    #[test]
    fn store_default_is_empty() {
        let store = NotificationStore::default();
        assert!(store.entries.is_empty());
    }

    #[test]
    fn entry_serde_roundtrip() {
        let entry = NotificationEntry {
            dedupe_key: None,
            id: 42,
            kind: NotificationKind::Permission,
            at: "2026-07-01T10:00:00+08:00".into(),
            title: "Test notification".into(),
            body: Some("details".into()),
            session_id: Some("sess-1".into()),
            request_id: Some("request-1".into()),
            automation_id: None,
            severity: "warn".into(),
            read: false,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: NotificationEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.title, "Test notification");
        assert_eq!(parsed.severity, "warn");
        assert_eq!(parsed.request_id.as_deref(), Some("request-1"));
        assert!(!parsed.read);
    }

    #[test]
    fn entry_deserialize_defaults() {
        // Missing optional fields should use defaults.
        let json = r#"{"id":1,"kind":"info","at":"2026-01-01","title":"hi"}"#;
        let entry: NotificationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.severity, "info");
        assert!(!entry.read);
        assert!(entry.body.is_none());
        assert!(entry.session_id.is_none());
    }

    #[test]
    fn notification_kind_uses_frontend_snake_case_and_reads_legacy_values() {
        assert_eq!(
            serde_json::to_string(&NotificationKind::FolderTrust).unwrap(),
            r#""folder_trust""#
        );
        let legacy: NotificationKind = serde_json::from_str(r#""folderTrust""#).unwrap();
        assert!(matches!(legacy, NotificationKind::FolderTrust));
        let legacy_hyphen: NotificationKind =
            serde_json::from_str(r#""session-complete""#).unwrap();
        assert!(matches!(legacy_hyphen, NotificationKind::SessionComplete));
    }

    #[test]
    fn external_channel_validation_accepts_real_delivery_kinds_only() {
        let webhook = NotifyChannel {
            id: "slack".into(),
            label: "Slack".into(),
            kind: ChannelKind::SlackWebhook,
            endpoint: Some("https://hooks.example.test/abc".into()),
            enabled: true,
        };
        assert!(validate_channel(&webhook).is_ok());
        assert!(validate_channel(&NotifyChannel {
            kind: ChannelKind::Email,
            endpoint: Some("person@example.test".into()),
            ..webhook.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            kind: ChannelKind::GenericWebhook,
            endpoint: Some("file:///tmp/not-a-webhook".into()),
            ..webhook.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            kind: ChannelKind::GenericWebhook,
            endpoint: Some("http://hooks.example.test/insecure".into()),
            ..webhook.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            kind: ChannelKind::GenericWebhook,
            endpoint: Some("http://127.0.0.1:8080/hook".into()),
            ..webhook
        })
        .is_ok());
    }

    #[test]
    fn editing_channel_without_replacement_keeps_existing_webhook_secret() {
        let original = NotifyChannel {
            id: "slack".into(),
            label: "Old name".into(),
            kind: ChannelKind::SlackWebhook,
            endpoint: Some("https://hooks.example.test/private-token".into()),
            enabled: true,
        };
        let mut store = ChannelStore {
            channels: vec![original.clone()],
        };
        upsert_channel_in_store(
            &mut store,
            NotifyChannel {
                label: "New name".into(),
                endpoint: None,
                ..original
            },
        )
        .unwrap();
        assert_eq!(store.channels[0].label, "New name");
        assert_eq!(
            store.channels[0].endpoint.as_deref(),
            Some("https://hooks.example.test/private-token")
        );
    }

    #[test]
    fn external_channel_validation_rejects_credentials_and_unbounded_fields() {
        let base = NotifyChannel {
            id: "webhook-1".into(),
            label: "Webhook".into(),
            kind: ChannelKind::GenericWebhook,
            endpoint: Some("https://hooks.example.test/path".into()),
            enabled: true,
        };
        assert!(validate_channel(&NotifyChannel {
            endpoint: Some("https://user:secret@hooks.example.test/path".into()),
            ..base.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            id: "bad\nid".into(),
            ..base.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            label: "x".repeat(MAX_CHANNEL_LABEL_CHARS + 1),
            ..base.clone()
        })
        .is_err());
        assert!(validate_channel(&NotifyChannel {
            endpoint: Some(format!(
                "https://example.test/{}",
                "x".repeat(MAX_ENDPOINT_CHARS)
            )),
            ..base
        })
        .is_err());
    }

    #[test]
    fn bounded_reader_rejects_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_bounded(&path, 4, "测试配置").is_err());
    }

    #[test]
    fn bounded_reader_rejects_non_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_bounded(dir.path(), 1024, "测试配置").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_and_transaction_lock_do_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside.json");
        let store = dir.path().join("channels.json");
        std::fs::write(&target, br#"{"channels":[]}"#).unwrap();
        symlink(&target, &store).unwrap();
        assert!(read_bounded(&store, 1024, "测试配置").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), br#"{"channels":[]}"#);

        std::fs::remove_file(&store).unwrap();
        let lock_target = dir.path().join("outside.lock");
        std::fs::write(&lock_target, b"unchanged").unwrap();
        symlink(&lock_target, lock_path(&store)).unwrap();
        let result = with_file_access_at(&store, channel_access(), "测试配置", || Ok(()));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&lock_target).unwrap(), b"unchanged");
    }

    #[test]
    fn persisted_channel_store_rejects_duplicate_ids_and_invalid_urls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.json");
        let channel = NotifyChannel {
            id: "duplicate".into(),
            label: "Webhook".into(),
            kind: ChannelKind::GenericWebhook,
            endpoint: Some("https://hooks.example.test/path".into()),
            enabled: true,
        };
        let duplicate = ChannelStore {
            channels: vec![channel.clone(), channel],
        };
        assert!(write_channels_at(&path, &duplicate).is_err());
        assert!(!path.exists());

        std::fs::write(
            &path,
            br#"{"channels":[{"id":"hook","label":"Hook","kind":"generic-webhook","endpoint":"https://example.test/hook#ignored","enabled":true}]}"#,
        )
        .unwrap();
        let persisted = read_channels_at(&path).unwrap();
        assert!(validate_channel(&persisted.channels[0]).is_err());
    }

    #[test]
    fn malformed_channel_store_is_reported_and_never_overwritten_by_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.json");
        let corrupt = br#"{"channels":["#;
        std::fs::write(&path, corrupt).unwrap();
        let result = update_channels_at(&path, |store| {
            store.channels.push(NotifyChannel {
                id: "new".into(),
                label: "New".into(),
                kind: ChannelKind::Desktop,
                endpoint: None,
                enabled: true,
            });
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    /// Subprocess target used by `channel_transactions_preserve_cross_process_updates`.
    #[test]
    fn channel_transaction_child_writer() {
        let Some(path) = std::env::var_os("ECHO_NOTIFY_TEST_CHILD_PATH").map(PathBuf::from) else {
            return;
        };
        let id = std::env::var("ECHO_NOTIFY_TEST_CHILD_ID").unwrap();
        update_channels_at(&path, |store| {
            store.channels.push(NotifyChannel {
                id: id.clone(),
                label: format!("Channel {id}"),
                kind: ChannelKind::Desktop,
                endpoint: None,
                enabled: true,
            });
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn channel_transactions_preserve_cross_process_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.json");
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for index in 0..6 {
            children.push(
                std::process::Command::new(&executable)
                    .arg("--exact")
                    .arg("notifications::tests::channel_transaction_child_writer")
                    .arg("--nocapture")
                    .env("ECHO_NOTIFY_TEST_CHILD_PATH", &path)
                    .env("ECHO_NOTIFY_TEST_CHILD_ID", format!("child-{index}"))
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let store = with_file_access_at(&path, channel_access(), "测试配置", || {
            read_channels_at(&path)
        })
        .unwrap();
        assert_eq!(store.channels.len(), 6);
        for index in 0..6 {
            assert!(store
                .channels
                .iter()
                .any(|channel| channel.id == format!("child-{index}")));
        }
    }

    #[test]
    fn outgoing_messages_are_bounded_and_severity_is_normalized() {
        let message = normalize_message(NotifyMessage {
            title: "t".repeat(MAX_TITLE_CHARS + 1),
            body: Some("b".repeat(MAX_BODY_CHARS + 1)),
            level: "fatal".into(),
            session_id: Some("s".repeat(MAX_SESSION_ID_CHARS + 1)),
            ..Default::default()
        });
        assert_eq!(message.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(message.title.len(), MAX_TITLE_CHARS);
        assert_eq!(message.body.unwrap().len(), MAX_BODY_BYTES);
        assert_eq!(message.level, "info");
        assert_eq!(
            message.session_id.unwrap().chars().count(),
            MAX_SESSION_ID_CHARS
        );

        let multibyte = normalize_message(NotifyMessage {
            title: "界".repeat(MAX_TITLE_CHARS),
            body: None,
            level: "info".into(),
            session_id: Some("界".repeat(MAX_SESSION_ID_CHARS)),
            ..Default::default()
        });
        assert!(multibyte.title.len() <= MAX_TITLE_BYTES);
        assert!(multibyte.session_id.unwrap().len() <= MAX_SESSION_ID_BYTES);
    }

    #[test]
    fn slack_payload_contains_title_and_body() {
        let value = payload(
            &NotifyChannel {
                id: "slack".into(),
                label: "Slack".into(),
                kind: ChannelKind::SlackWebhook,
                endpoint: Some("https://hooks.example.test/abc".into()),
                enabled: true,
            },
            &NotifyMessage {
                title: "Task complete".into(),
                body: Some("The scheduled task finished".into()),
                level: "info".into(),
                session_id: Some("session-1".into()),
                ..Default::default()
            },
        );
        assert!(value["text"].as_str().unwrap().contains("Task complete"));
        assert!(value["blocks"][0]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("scheduled task finished"));
    }

    #[test]
    fn webhook_payloads_do_not_expand_agent_authored_mass_mentions() {
        let message = NotifyMessage {
            title: "<!channel> <@everyone>".into(),
            body: None,
            level: "warn".into(),
            session_id: None,
            ..Default::default()
        };
        let slack = payload(
            &NotifyChannel {
                id: "slack".into(),
                label: "Slack".into(),
                kind: ChannelKind::SlackWebhook,
                endpoint: Some("https://hooks.example.test/abc".into()),
                enabled: true,
            },
            &message,
        );
        assert!(!slack["text"].as_str().unwrap().contains("<!channel>"));

        let discord = payload(
            &NotifyChannel {
                id: "discord".into(),
                label: "Discord".into(),
                kind: ChannelKind::DiscordWebhook,
                endpoint: Some("https://hooks.example.test/abc".into()),
                enabled: true,
            },
            &message,
        );
        assert_eq!(discord["allowed_mentions"]["parse"], serde_json::json!([]));
    }
    #[test]
    fn automation_inbox_append_once_survives_reload_and_keeps_normal_notifications() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("notifications.json");
        let first = append_once_at(
            &path,
            NotificationKind::SessionComplete,
            "done",
            Some("result"),
            Some("session"),
            None,
            Some("automation"),
            "info",
            Some("automation-terminal:record"),
        )
        .unwrap();
        // A restart loses all process state; only the file is used for the next claim.
        let second = append_once_at(
            &path,
            NotificationKind::SessionComplete,
            "done",
            Some("result"),
            Some("session"),
            None,
            Some("automation"),
            "info",
            Some("automation-terminal:record"),
        )
        .unwrap();
        assert_eq!(first, second);
        append_once_at(
            &path,
            NotificationKind::SessionComplete,
            "ordinary followup",
            None,
            Some("session"),
            None,
            None,
            "info",
            None,
        )
        .unwrap();
        let store = read_store_at(&path).unwrap();
        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].request_id, None);
        assert_eq!(
            store.entries[0].dedupe_key.as_deref(),
            Some("automation-terminal:record")
        );
    }
}
