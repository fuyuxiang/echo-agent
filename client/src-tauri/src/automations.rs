//! Automations (定时任务) — EchoAgent-managed local scheduler.
//!
//! EchoAgent only exposes `echo.agent/scheduler/delete` (deleting tasks it created itself
//! via tool calls). It does NOT let a client create new scheduled tasks.
//! EchoAgent's automation panel needs create/update/list, so EchoAgent keeps
//! its own automation table in `~/.echo-agent/echoagent-automations.json` and a run
//! record table in `~/.echo-agent/echoagent-automation-records.json`.
//!
//! Data model mirrors EchoAgent's automation panel 1:1:
//!  - scheduleType: "recurring" | "once"
//!  - recurring schedule: freq DAILY/WEEKLY/MONTHLY/YEARLY/HOURLY + interval
//!    (双周 = WEEKLY interval 2; 按间隔 = HOURLY + intervalHours) + byday /
//!    bymonthday / bymonth + byhour:byminute
//!  - once: scheduledDate (YYYY-MM-DD) + scheduledTime (HH:MM)
//!  - validity window: validFromDate / validUntilDate (recurring only)
//!  - extras: skills, expert, connectorIds, permissionMode, pushToWeChat
//!
//! At fire time the scheduler opens a fresh EchoAgent session in the automation's
//! cwd and sends the prompt; a run record (running → success/failed) is
//! written so the 运行记录 tab can render history.
//!
//! The scheduler runs in-process (tokio task). A short polling interval keeps
//! minute-precision schedules responsive while the persisted occurrence claim
//! still guarantees that a due run is dispatched only once.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use tauri_plugin_autostart::ManagerExt;

use crate::commands::AppState;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

/// Hard upper bound for one unattended Agent turn. A scheduler must never be
/// held indefinitely by a missing permission response, a wedged provider, or a
/// tool process that forgot to terminate.
const AUTOMATION_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const MAX_CONCURRENT_AUTOMATION_RUNS: usize = 3;
const AUTOMATION_SCHEDULER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

// ---------- models ----------

/// RRULE-like frequency. Serialized as "DAILY" | "WEEKLY" | "MONTHLY" |
/// "YEARLY" | "HOURLY" to match the frontend model.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleFreq {
    DAILY,
    WEEKLY,
    MONTHLY,
    YEARLY,
    HOURLY,
}

fn default_interval() -> u32 {
    1
}
fn default_hour() -> u32 {
    9
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSchedule {
    pub freq: ScheduleFreq,
    /// 1 = every week, 2 = bi-weekly (WEEKLY only).
    #[serde(default = "default_interval")]
    pub interval: u32,
    /// Weekday codes "MO".."SU".
    #[serde(default)]
    pub byday: Vec<String>,
    /// Days of month 1..=31 (MONTHLY/YEARLY).
    #[serde(default)]
    pub bymonthday: Vec<u32>,
    /// Months 1..=12 (YEARLY).
    #[serde(default)]
    pub bymonth: Vec<u32>,
    #[serde(default = "default_hour")]
    pub byhour: u32,
    #[serde(default)]
    pub byminute: u32,
    /// 按间隔: every N hours (HOURLY).
    #[serde(default = "default_interval")]
    pub interval_hours: u32,
}

impl Default for AutomationSchedule {
    fn default() -> Self {
        Self {
            freq: ScheduleFreq::DAILY,
            interval: 1,
            byday: ALL_DAYS.iter().map(|s| s.to_string()).collect(),
            bymonthday: vec![],
            bymonth: vec![],
            byhour: 9,
            byminute: 0,
            interval_hours: 1,
        }
    }
}

const ALL_DAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub name: String,
    /// The prompt to send when this automation fires.
    pub prompt: String,
    /// Comma-separated workspace directories (first entry is the run cwd).
    #[serde(default)]
    pub cwds: String,
    /// "ACTIVE" | "PAUSED".
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_is_thinking: bool,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub expert_id: Option<String>,
    #[serde(default)]
    pub expert_name: Option<String>,
    /// Stable definition identity; legacy name-only references require reselection.
    #[serde(default)]
    pub expert_path: Option<String>,
    #[serde(default)]
    pub connector_ids: Vec<String>,
    /// "fullAccess" | "default".
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// "recurring" | "once".
    #[serde(default = "default_schedule_type")]
    pub schedule_type: String,
    #[serde(default)]
    pub schedule: AutomationSchedule,
    /// Once mode: YYYY-MM-DD.
    #[serde(default)]
    pub scheduled_date: Option<String>,
    /// Once mode: HH:MM.
    #[serde(default)]
    pub scheduled_time: Option<String>,
    /// Recurring validity window (YYYY-MM-DD, inclusive).
    #[serde(default)]
    pub valid_from_date: Option<String>,
    #[serde(default)]
    pub valid_until_date: Option<String>,
    #[serde(default)]
    pub push_to_we_chat: bool,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub next_run_at: Option<String>,
    pub created_at: String,
}

fn default_status() -> String {
    "ACTIVE".into()
}
fn default_permission_mode() -> String {
    "default".into()
}
fn default_schedule_type() -> String {
    "recurring".into()
}

/// A single run-history entry (运行记录 / inbox item).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunRecord {
    pub id: String,
    pub automation_id: String,
    pub automation_name: String,
    /// "queued" | "running" | "success" | "failed" | "interrupted".
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Identity of this run's first prompt; historical sessions may be reused.
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub notification_delivery: Option<String>,
    #[serde(default)]
    pub completion_pending: bool,
    /// Workspace used by this run. Persisted so a run can be opened after a
    /// restart without guessing the session's directory from renderer state.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Model selected when the run was dispatched.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Immutable execution input captured when the occurrence was claimed.
    /// Recovery must not silently run a later edit under an older queue row.
    #[serde(default)]
    pub automation_snapshot: Option<Automation>,
    /// Persisted due timestamp for scheduled runs. This is an occurrence key,
    /// useful for diagnosing/recovering a crash between claim and dispatch.
    #[serde(default)]
    pub scheduled_for: Option<String>,
    /// Human-readable dispatch/runtime failure reason.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AutomationStore {
    #[serde(default)]
    pub automations: Vec<Automation>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RunRecordStore {
    #[serde(default)]
    pub records: Vec<AutomationRunRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSnapshot {
    pub automations: Vec<Automation>,
    pub records: Vec<AutomationRunRecord>,
}

/// Small push event used to invalidate renderer snapshots and immediately
/// hydrate a newly-created background session in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationUpdateEvent {
    pub phase: String,
    pub automation_id: String,
    pub automation_name: String,
    pub record_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Concrete model bound to this newly created session, never the Auto sentinel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn emit_automation_update(app: &AppHandle, event: AutomationUpdateEvent) {
    if let Err(error) = app.emit("agent://automation-update", &event) {
        tracing::warn!(%error, record_id = %event.record_id, "failed to emit automation lifecycle update");
    }
}

enum RunUpdate<'a> {
    Queued,
    Running,
    SessionCreated(&'a str, &'a str),
    Failed {
        session_id: Option<&'a str>,
        error: &'a str,
    },
}

fn emit_run_update(
    app: &AppHandle,
    automation: &Automation,
    record_id: &str,
    cwd: &Path,
    update: RunUpdate<'_>,
) {
    emit_automation_update(app, run_update_event(automation, record_id, cwd, update));
}

fn run_update_event(
    automation: &Automation,
    record_id: &str,
    cwd: &Path,
    update: RunUpdate<'_>,
) -> AutomationUpdateEvent {
    let current_model_id = match &update {
        RunUpdate::SessionCreated(_, model_id) => Some((*model_id).to_string()),
        _ => None,
    };
    let (phase, status, session_id, error) = match update {
        RunUpdate::Queued => ("queued", "queued", None, None),
        RunUpdate::Running => ("running", "running", None, None),
        RunUpdate::SessionCreated(session_id, _) => {
            ("sessionCreated", "running", Some(session_id), None)
        }
        RunUpdate::Failed { session_id, error } => ("finished", "failed", session_id, Some(error)),
    };
    AutomationUpdateEvent {
        current_model_id,
        phase: phase.into(),
        automation_id: automation.id.clone(),
        automation_name: automation.name.clone(),
        record_id: record_id.into(),
        status: status.into(),
        session_id: session_id.map(str::to_string),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        error: error.map(str::to_string),
    }
}

// ---------- persistence ----------

fn store_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-automations.json")
}
fn records_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-automation-records.json")
}

static STORE_ACCESS: OnceLock<Mutex<()>> = OnceLock::new();
static RECORD_ACCESS: OnceLock<Mutex<()>> = OnceLock::new();
static FULL_ACCESS_SESSIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static AUTOMATION_RUN_SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn store_access() -> &'static Mutex<()> {
    STORE_ACCESS.get_or_init(|| Mutex::new(()))
}

fn record_access() -> &'static Mutex<()> {
    RECORD_ACCESS.get_or_init(|| Mutex::new(()))
}

fn full_access_sessions() -> &'static Mutex<HashSet<String>> {
    FULL_ACCESS_SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn automation_run_slots() -> &'static Arc<tokio::sync::Semaphore> {
    AUTOMATION_RUN_SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_AUTOMATION_RUNS)))
}

/// Serialize occurrences of the same automation while still allowing the
/// scheduler to persist and display a due occurrence immediately. Weak values
/// keep deleted automation ids from accumulating for the lifetime of the app.
fn automation_run_lock(automation_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if let Some(lock) = locks.get(automation_id).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(automation_id.to_string(), Arc::downgrade(&lock));
    lock
}

pub fn has_active_automations() -> bool {
    let _guard = store_access().lock().unwrap();
    match read_store() {
        Ok(store) => store
            .automations
            .iter()
            .any(|automation| automation.status == "ACTIVE"),
        Err(error) => {
            // A damaged/unreadable schedule must not make the resident process
            // silently exit and miss tasks before the user can repair it.
            tracing::error!(%error, "failed to inspect automation store");
            true
        }
    }
}

fn with_autostart_write_permission<T>(
    review: Option<&str>,
    packaged_validation: Option<&str>,
    operation: impl FnOnce() -> T,
) -> Option<T> {
    // These explicit acceptance modes must never change the user's OS login
    // item, whose plugin key is the package name rather than the profile ID.
    if review == Some("1") || packaged_validation == Some("1") {
        return None;
    }
    Some(operation())
}

fn sync_autostart_enabled(app: &AppHandle, enabled: bool) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let review = std::env::var("ECHO_DESKTOP_REVIEW").ok();
        let packaged_validation = std::env::var("ECHO_DESKTOP_VALIDATION").ok();
        with_autostart_write_permission(review.as_deref(), packaged_validation.as_deref(), || {
            let manager = app.autolaunch();
            let result = if enabled {
                manager.enable()
            } else {
                manager.disable()
            };
            if let Err(error) = result {
                tracing::warn!(%error, enabled, "failed to synchronize automation autostart");
            }
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let _ = (app, enabled);
}

pub fn sync_autostart_state(app: &AppHandle) {
    sync_autostart_enabled(app, has_active_automations());
}

fn write_json(path: &Path, body: &str) -> Result<(), String> {
    crate::paths::write_private_file(path, body.as_bytes())
}

const MAX_AUTOMATION_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AUTOMATION_RECORD_BYTES: u64 = 32 * 1024 * 1024;
const MAX_AUTOMATION_SKILL_BYTES: u64 = 2 * 1024 * 1024;

fn read_bounded_string(path: &Path, max_bytes: u64) -> Result<Option<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{} must be a regular, non-symlink file",
            path.display()
        ));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "{} exceeds the {} byte safety limit",
            path.display(),
            max_bytes
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("inspect opened {}: {error}", path.display()))?
        .is_file()
    {
        return Err(format!("{} changed while opening", path.display()));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{} exceeds the {} byte safety limit",
            path.display(),
            max_bytes
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("{} is not valid UTF-8: {error}", path.display()))
}

/// Read the automation store. A missing file is an empty store; unreadable or
/// corrupt data is an error so a later mutation cannot silently erase it.
/// Accepts the legacy v1 shape ({schedule:{type:"daily"|...}}, lowercase
/// status, single `cwd` string) and migrates it in memory.
fn read_store() -> Result<AutomationStore, String> {
    let path = store_path();
    let content = match read_bounded_string(&path, MAX_AUTOMATION_STORE_BYTES)? {
        Some(content) => content,
        None => return Ok(AutomationStore::default()),
    };
    if let Ok(mut store) = serde_json::from_str::<AutomationStore>(&content) {
        migrate_builtin_models(&mut store);
        return Ok(store);
    }
    // Legacy fallback: reshape each automation object, then parse.
    let mut value = serde_json::from_str::<serde_json::Value>(&content)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    migrate_legacy_json(&mut value);
    let mut store = serde_json::from_value(value)
        .map_err(|error| format!("migrate {}: {error}", path.display()))?;
    migrate_builtin_models(&mut store);
    Ok(store)
}

fn migrate_builtin_models(store: &mut AutomationStore) {
    for automation in &mut store.automations {
        if automation
            .model_id
            .as_deref()
            .is_some_and(crate::providers::is_legacy_builtin_model_id)
        {
            automation.model_id = Some(crate::providers::BUILTIN_DEFAULT_MODEL_ID.into());
        }
    }
}

/// Convert legacy v1 automation objects to the current model, in place.
fn migrate_legacy_json(root: &mut serde_json::Value) {
    let Some(items) = root.get_mut("automations").and_then(|a| a.as_array_mut()) else {
        return;
    };
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        // status: "active"|"paused" → "ACTIVE"|"PAUSED"
        if let Some(status) = obj.get("status").and_then(|s| s.as_str()) {
            let upper = status.to_uppercase();
            obj.insert("status".into(), serde_json::Value::String(upper));
        }
        // cwd → cwds
        if !obj.contains_key("cwds") {
            if let Some(cwd) = obj.get("cwd").and_then(|c| c.as_str()) {
                obj.insert("cwds".into(), serde_json::Value::String(cwd.to_string()));
            }
        }
        obj.remove("cwd");
        // schedule {type: ...} → {freq: ...} (+ once fields)
        let Some(schedule) = obj.get("schedule").cloned() else {
            continue;
        };
        if schedule.get("freq").is_some() {
            continue; // already current
        }
        let Some(kind) = schedule.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        let parse_time = |key: &str| -> (u32, u32) {
            schedule
                .get(key)
                .and_then(|t| t.as_str())
                .and_then(|t| {
                    let mut parts = t.split(':');
                    let h = parts.next()?.parse().ok()?;
                    let m = parts.next()?.parse().ok()?;
                    Some((h, m))
                })
                .unwrap_or((9, 0))
        };
        let mut new_schedule = serde_json::json!({
            "freq": "DAILY",
            "interval": 1,
            "byday": ALL_DAYS,
            "bymonthday": [],
            "bymonth": [],
            "byhour": 9,
            "byminute": 0,
            "intervalHours": 1,
        });
        match kind {
            "once" => {
                obj.insert(
                    "scheduleType".into(),
                    serde_json::Value::String("once".into()),
                );
                if let Some(at) = schedule.get("at").and_then(|t| t.as_str()) {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(at) {
                        let local = dt.with_timezone(&Local);
                        obj.insert(
                            "scheduledDate".into(),
                            serde_json::Value::String(local.format("%Y-%m-%d").to_string()),
                        );
                        obj.insert(
                            "scheduledTime".into(),
                            serde_json::Value::String(local.format("%H:%M").to_string()),
                        );
                    }
                }
            }
            "daily" => {
                let (h, m) = parse_time("time");
                new_schedule["byhour"] = h.into();
                new_schedule["byminute"] = m.into();
            }
            "weekly" => {
                let (h, m) = parse_time("time");
                new_schedule["freq"] = "WEEKLY".into();
                new_schedule["byhour"] = h.into();
                new_schedule["byminute"] = m.into();
                // legacy weekdays: 0=Sunday .. 6=Saturday
                let codes: Vec<&str> = schedule
                    .get("weekdays")
                    .and_then(|w| w.as_array())
                    .map(|days| {
                        days.iter()
                            .filter_map(|d| d.as_u64())
                            .filter_map(|d| {
                                ["SU", "MO", "TU", "WE", "TH", "FR", "SA"]
                                    .get(d as usize)
                                    .copied()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !codes.is_empty() {
                    new_schedule["byday"] = serde_json::json!(codes);
                }
            }
            "monthly" => {
                let (h, m) = parse_time("time");
                new_schedule["freq"] = "MONTHLY".into();
                new_schedule["byhour"] = h.into();
                new_schedule["byminute"] = m.into();
                if let Some(day) = schedule.get("day").and_then(|d| d.as_u64()) {
                    new_schedule["bymonthday"] = serde_json::json!([day]);
                }
            }
            _ => {}
        }
        obj.insert("schedule".into(), new_schedule);
    }
}

fn write_store(store: &AutomationStore) -> Result<(), String> {
    let body =
        serde_json::to_string_pretty(store).map_err(|e| format!("serialize automations: {e}"))?;
    if body.len() as u64 > MAX_AUTOMATION_STORE_BYTES {
        return Err("自动化任务数据超过 8 MiB 安全上限".into());
    }
    write_json(&store_path(), &body)
}

fn read_records() -> Result<RunRecordStore, String> {
    let path = records_path();
    let content = match read_bounded_string(&path, MAX_AUTOMATION_RECORD_BYTES)? {
        Some(content) => content,
        None => return Ok(RunRecordStore::default()),
    };
    serde_json::from_str(&content).map_err(|error| format!("parse {}: {error}", path.display()))
}

const MAX_RECORDS: usize = 500;

fn write_records(store: &mut RunRecordStore) -> Result<(), String> {
    // Keep history bounded without ever evicting an unfinished run. Dropping a
    // queued/running row would also drop our durable dispatch/recovery marker.
    while store.records.len() > MAX_RECORDS {
        let Some(index) = store.records.iter().position(|record| {
            matches!(record.status.as_str(), "success" | "failed" | "interrupted")
                && !pending_terminals().lock().unwrap().contains_key(&record.id)
        }) else {
            break;
        };
        store.records.remove(index);
    }
    let body =
        serde_json::to_string_pretty(store).map_err(|e| format!("serialize records: {e}"))?;
    if body.len() as u64 > MAX_AUTOMATION_RECORD_BYTES {
        return Err("自动化运行记录超过 32 MiB 安全上限".into());
    }
    write_json(&records_path(), &body)
}

fn append_run_started(
    records: &mut RunRecordStore,
    automation: &Automation,
    started_at: &str,
    cwd: &Path,
    scheduled_for: Option<&str>,
    resolved_model_id: Option<&str>,
) -> String {
    let id = uuid::Uuid::now_v7().to_string();
    records.records.push(AutomationRunRecord {
        id: id.clone(),
        automation_id: automation.id.clone(),
        automation_name: automation.name.clone(),
        status: "queued".into(),
        started_at: started_at.into(),
        finished_at: None,
        session_id: None,
        prompt_id: None,
        notification_delivery: None,
        completion_pending: false,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        model_id: resolved_model_id
            .map(str::to_string)
            .or_else(|| automation.model_id.clone()),
        automation_snapshot: Some(automation.clone()),
        scheduled_for: scheduled_for.map(str::to_string),
        error: None,
        archived: false,
    });
    id
}

/// Durably append a queued dispatch and return its id.
fn record_run_started(
    automation: &Automation,
    started_at: &str,
    cwd: &Path,
    scheduled_for: Option<&str>,
    resolved_model_id: &str,
) -> Result<String, String> {
    let _guard = record_access().lock().unwrap();
    let mut records = read_records()?;
    if records.records.iter().any(|record| {
        record.automation_id == automation.id
            && matches!(record.status.as_str(), "queued" | "running")
    }) {
        return Err("该自动化任务已在运行，请等待完成后再试".into());
    }
    let id = append_run_started(
        &mut records,
        automation,
        started_at,
        cwd,
        scheduled_for,
        Some(resolved_model_id),
    );
    write_records(&mut records)?;
    Ok(id)
}

/// Link the newly-created session while the automation is still running.
fn record_run_session(record_id: &str, session_id: &str, prompt_id: &str) -> Result<(), String> {
    let _guard = record_access().lock().unwrap();
    let mut records = read_records()?;
    let record = records
        .records
        .iter_mut()
        .find(|record| record.id == record_id)
        .ok_or_else(|| format!("automation record {record_id} not found"))?;
    record.session_id = Some(session_id.into());
    record.prompt_id = Some(prompt_id.into());
    write_records(&mut records)?;
    active_run_prompts()
        .lock()
        .unwrap()
        .insert((session_id.into(), prompt_id.into()), record_id.into());
    Ok(())
}

fn mark_record_running(record_id: &str, resolved_model_id: &str) -> Result<(), String> {
    let _guard = record_access().lock().unwrap();
    let mut records = read_records()?;
    let record = records
        .records
        .iter_mut()
        .find(|record| record.id == record_id)
        .ok_or_else(|| format!("automation record {record_id} not found"))?;
    match record.status.as_str() {
        "queued" => {
            record.status = "running".into();
            // Auto is resolved at dispatch time. Persist the concrete id so the
            // history row remains an accurate audit record after defaults change.
            record.model_id = Some(resolved_model_id.to_string());
        }
        status => {
            return Err(format!(
                "automation record {record_id} is not dispatchable ({status})"
            ));
        }
    }
    write_records(&mut records)
}

fn record_session_id(record_id: &str) -> Result<Option<String>, String> {
    let _guard = record_access().lock().unwrap();
    Ok(read_records()?
        .records
        .into_iter()
        .find(|record| record.id == record_id)
        .and_then(|record| record.session_id))
}

fn finalize_record(
    record: &mut AutomationRunRecord,
    ok: bool,
    finished_at: &str,
    session_id: Option<&str>,
    error: Option<&str>,
) -> bool {
    if !matches!(record.status.as_str(), "queued" | "running") {
        return false;
    }
    record.status = if ok { "success" } else { "failed" }.into();
    record.finished_at = Some(finished_at.to_string());
    record.error = (!ok).then(|| error.unwrap_or("自动化执行失败").to_string());
    record.automation_snapshot = None;
    if let Some(session_id) = session_id {
        record.session_id = Some(session_id.into());
    }
    true
}

/// Finalize a record as success/failed. The session was linked at dispatch time.
fn record_run_finished(
    record_id: &str,
    ok: bool,
    session_id: Option<&str>,
    error: Option<&str>,
) -> bool {
    let _guard = record_access().lock().unwrap();
    if read_records()
        .ok()
        .and_then(|store| store.records.into_iter().find(|r| r.id == record_id))
        .is_some_and(|record| !matches!(record.status.as_str(), "queued" | "running"))
        && !pending_terminals().lock().unwrap().contains_key(record_id)
    {
        return false;
    }
    let fact = TerminalFact {
        record_id: record_id.into(),
        session_id: session_id.map(str::to_string),
        prompt_id: None,
        interrupted: false,
        ok,
        error: error.map(str::to_string),
        finished_at: Local::now().to_rfc3339(),
        completion: None,
        local_notification_id: None,
        external_claimed: false,
        external_result: None,
        restored_orphan: false,
    };
    match persist_terminal(fact) {
        Ok(completion) => completion.is_some(),
        Err(error) => {
            tracing::error!(%error, %record_id, "automation terminal pending retry");
            false
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationCompletion {
    pub push: bool,
    pub automation_name: String,
    pub event: AutomationUpdateEvent,
}

/// Completion identity is prompt-scoped, never inferred from session history.
#[derive(Debug)]
pub enum AutomationCompletionOutcome {
    NotAutomation,
    AlreadyFinalized,
    Persisted(Box<AutomationCompletion>),
    PersistencePending(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TerminalFact {
    record_id: String,
    session_id: Option<String>,
    prompt_id: Option<String>,
    ok: bool,
    #[serde(default)]
    interrupted: bool,
    error: Option<String>,
    finished_at: String,
    #[serde(default)]
    completion: Option<AutomationCompletion>,
    #[serde(default)]
    local_notification_id: Option<u64>,
    #[serde(default)]
    external_claimed: bool,
    #[serde(default)]
    external_result: Option<String>,
    #[serde(default)]
    restored_orphan: bool,
}

/// Reconcile a staged backup before any service can retry an outbox. Restored
/// work never runs again and external delivery is never replayed by restore.
pub(crate) fn prepare_restored_terminals(home: &Path) -> Result<(), String> {
    let history_path = home.join("echoagent-automation-records.json");
    let mut history: serde_json::Value = if history_path.exists() {
        serde_json::from_slice(&crate::shell_fs::read_regular_file_bounded(
            &history_path,
            64 * 1024 * 1024,
        )?)
        .map_err(|e| e.to_string())?
    } else {
        serde_json::json!({"records":[]})
    };
    let records = history
        .get_mut("records")
        .and_then(|v| v.as_array_mut())
        .ok_or("自动化历史格式无效")?;
    let directory = home.join("automation-terminal-outbox");
    if directory.exists() {
        for entry in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let bytes = crate::shell_fs::read_regular_file_bounded(&entry.path(), 4 * 1024 * 1024)?;
            let mut fact: TerminalFact =
                serde_json::from_slice(&bytes).map_err(|e| format!("自动化完成事实损坏：{e}"))?;
            let expected_name = format!("{:x}.json", Sha256::digest(fact.record_id.as_bytes()));
            if entry.file_name() != std::ffi::OsStr::new(&expected_name) {
                return Err("自动化完成事实文件身份不一致".into());
            }
            let Some(value) = records
                .iter_mut()
                .find(|v| v["id"].as_str() == Some(&fact.record_id))
            else {
                // A deleted terminal history can legitimately leave a pending
                // fact. Preserve it, but never resurrect or publish that run.
                fact.restored_orphan = true;
                fact.external_claimed = true;
                fact.external_result = Some("由备份恢复，历史已删除，未重建或补发".into());
                crate::paths::write_private_file(
                    &entry.path(),
                    &serde_json::to_vec(&fact).map_err(|e| e.to_string())?,
                )?;
                continue;
            };
            let mut record: AutomationRunRecord =
                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            if fact
                .session_id
                .as_ref()
                .is_some_and(|id| record.session_id.as_ref() != Some(id))
                || fact
                    .prompt_id
                    .as_ref()
                    .is_some_and(|id| record.prompt_id.as_ref() != Some(id))
            {
                return Err("自动化恢复事实轮次不一致".into());
            }
            finalize_from_fact(&mut record, &fact);
            let expected_status = if fact.interrupted {
                "interrupted"
            } else if fact.ok {
                "success"
            } else {
                "failed"
            };
            if record.status != expected_status {
                return Err("自动化恢复事实与终态历史冲突，原数据已保留".into());
            }
            record.automation_snapshot = None;
            let mut completion = completion_for_record(&record);
            completion.push = false;
            fact.completion = Some(completion);
            fact.external_claimed = true;
            fact.external_result = Some("由备份恢复，仅恢复本地结果，未补发外部通知".into());
            crate::paths::write_private_file(
                &entry.path(),
                &serde_json::to_vec(&fact).map_err(|e| e.to_string())?,
            )?;
            *value = serde_json::to_value(record).map_err(|e| e.to_string())?;
        }
    }
    for record in records {
        if matches!(record["status"].as_str(), Some("queued" | "running")) {
            record["status"] = serde_json::json!("failed");
            record["error"] = serde_json::json!("由备份恢复，未重新执行；请检查历史结果后手动运行");
            record["finishedAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
            record["automationSnapshot"] = serde_json::Value::Null;
        }
    }
    if history_path.exists() {
        crate::paths::write_private_file(
            &history_path,
            &serde_json::to_vec(&history).map_err(|e| e.to_string())?,
        )?;
    }
    Ok(())
}

fn active_run_prompts() -> &'static Mutex<HashMap<(String, String), String>> {
    static ACTIVE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn pending_terminals() -> &'static Mutex<HashMap<String, TerminalFact>> {
    static PENDING: OnceLock<Mutex<HashMap<String, TerminalFact>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}
fn terminal_dir() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("automation-terminal-outbox")
}
fn terminal_path(record_id: &str) -> Result<PathBuf, String> {
    if record_id.is_empty() || record_id.len() > 256 {
        return Err("invalid automation record identity".into());
    }
    // File names never include a deserialized path component.
    let encoded = format!("{:x}", Sha256::digest(record_id.as_bytes()));
    Ok(terminal_dir().join(format!("{encoded}.json")))
}
fn matches_prompt(record: &AutomationRunRecord, session_id: &str, prompt_id: &str) -> bool {
    !prompt_id.is_empty()
        && record.session_id.as_deref() == Some(session_id)
        && record.prompt_id.as_deref() == Some(prompt_id)
}
fn apply_terminal_fact(records: &mut RunRecordStore, fact: &TerminalFact) -> Result<bool, String> {
    let record = records
        .records
        .iter_mut()
        .find(|record| record.id == fact.record_id)
        .ok_or_else(|| "自动化完成记录不存在，保留待恢复事实".to_string())?;
    if let (Some(session), Some(prompt)) = (&fact.session_id, &fact.prompt_id) {
        if !matches_prompt(record, session, prompt) {
            return Err("自动化完成轮次与记录不一致".into());
        }
    }
    Ok(finalize_from_fact(record, fact))
}
fn finalize_from_fact(record: &mut AutomationRunRecord, fact: &TerminalFact) -> bool {
    let changed = finalize_record(
        record,
        fact.ok,
        &fact.finished_at,
        fact.session_id.as_deref(),
        fact.error.as_deref(),
    );
    if changed && fact.interrupted {
        record.status = "interrupted".into();
    }
    changed
}

fn completion_for_record(record: &AutomationRunRecord) -> AutomationCompletion {
    // Snapshot preference survives editing/deleting an automation during a run.
    let push = record
        .automation_snapshot
        .as_ref()
        .map(|a| a.push_to_we_chat)
        .unwrap_or(false);
    AutomationCompletion {
        push,
        automation_name: record.automation_name.clone(),
        event: AutomationUpdateEvent {
            current_model_id: None,
            phase: "finished".into(),
            automation_id: record.automation_id.clone(),
            automation_name: record.automation_name.clone(),
            record_id: record.id.clone(),
            status: record.status.clone(),
            session_id: record.session_id.clone(),
            cwd: record.cwd.clone(),
            error: record.error.clone(),
        },
    }
}
fn write_terminal_fact(fact: &TerminalFact) -> Result<(), String> {
    write_json(
        &terminal_path(&fact.record_id)?,
        &serde_json::to_string(fact).map_err(|e| e.to_string())?,
    )
}

/// Pure transition seam: durable fact precedes history; a history failure leaves
/// the exact original terminal outcome available to a fresh process.
fn persist_terminal_transition(
    records: &mut RunRecordStore,
    fact: &mut TerminalFact,
    mut save_fact: impl FnMut(&TerminalFact) -> Result<(), String>,
    mut save_history: impl FnMut(&mut RunRecordStore) -> Result<(), String>,
) -> Result<(), String> {
    if fact.completion.is_none() {
        let mut terminal = records
            .records
            .iter()
            .find(|record| record.id == fact.record_id)
            .cloned()
            .ok_or_else(|| "自动化完成记录不存在，保留待恢复事实".to_string())?;
        let push = terminal
            .automation_snapshot
            .as_ref()
            .is_some_and(|a| a.push_to_we_chat);
        finalize_from_fact(&mut terminal, fact);
        let mut completion = completion_for_record(&terminal);
        completion.push = push;
        fact.completion = Some(completion);
    }
    save_fact(fact)?;
    if apply_terminal_fact(records, fact)? {
        save_history(records)?;
    }
    Ok(())
}

/// Called with record_access held. Retain the outbox until local publication
/// and the external-attempt disposition are durably recorded.
fn persist_terminal(fact: TerminalFact) -> Result<Option<AutomationCompletion>, String> {
    let mut fact = pending_terminals()
        .lock()
        .unwrap()
        .entry(fact.record_id.clone())
        .or_insert(fact)
        .clone();
    // Even if history cannot be read, a known current prompt's fact survives.
    write_terminal_fact(&fact)?;
    let mut records = read_records()?;
    let result =
        persist_terminal_transition(&mut records, &mut fact, write_terminal_fact, write_records);
    pending_terminals()
        .lock()
        .unwrap()
        .insert(fact.record_id.clone(), fact.clone());
    result?;
    active_run_prompts()
        .lock()
        .unwrap()
        .retain(|_, id| id != &fact.record_id);
    Ok(fact.completion)
}

pub fn complete_run_for_session(
    session_id: &str,
    prompt_id: &str,
    ok: bool,
    error: Option<&str>,
) -> AutomationCompletionOutcome {
    complete_run_prompt(session_id, prompt_id, ok, error, false)
}

/// Manual takeover ends this scheduled occurrence. The replacement message is
/// an ordinary turn; historical follow-ups cannot claim or reopen this record.
pub fn interrupt_run_for_session(session_id: &str, prompt_id: &str) -> AutomationCompletionOutcome {
    complete_run_prompt(
        session_id,
        prompt_id,
        false,
        Some("本次自动化已结束，后续消息按普通会话继续执行"),
        true,
    )
}

fn complete_run_prompt(
    session_id: &str,
    prompt_id: &str,
    ok: bool,
    error: Option<&str>,
    interrupted: bool,
) -> AutomationCompletionOutcome {
    let _guard = record_access().lock().unwrap();
    let known_id = active_run_prompts()
        .lock()
        .unwrap()
        .get(&(session_id.into(), prompt_id.into()))
        .cloned();
    let record = match read_records() {
        Ok(records) => records
            .records
            .into_iter()
            .find(|r| matches_prompt(r, session_id, prompt_id)),
        Err(read_error) if known_id.is_none() => {
            // A broken history file must not classify unrelated ordinary turns as automation.
            tracing::error!(%read_error, "automation completion lookup unavailable");
            return AutomationCompletionOutcome::NotAutomation;
        }
        Err(_) => None,
    };
    if record.as_ref().is_some_and(|r| {
        !matches!(r.status.as_str(), "running" | "queued")
            && !pending_terminals().lock().unwrap().contains_key(&r.id)
    }) {
        return AutomationCompletionOutcome::AlreadyFinalized;
    }
    let Some(record_id) = record.map(|r| r.id).or(known_id) else {
        return AutomationCompletionOutcome::NotAutomation;
    };
    let fact = TerminalFact {
        record_id,
        session_id: Some(session_id.into()),
        prompt_id: Some(prompt_id.into()),
        interrupted,
        ok,
        error: error.map(str::to_string),
        finished_at: Local::now().to_rfc3339(),
        completion: None,
        local_notification_id: None,
        external_claimed: false,
        external_result: None,
        restored_orphan: false,
    };
    match persist_terminal(fact) {
        Ok(Some(completion)) => AutomationCompletionOutcome::Persisted(Box::new(completion)),
        Ok(None) => AutomationCompletionOutcome::AlreadyFinalized,
        Err(error) => AutomationCompletionOutcome::PersistencePending(error),
    }
}

/// Bounded-frequency retries persist facts only; they never dispatch a prompt again.
fn retry_terminal_completions(app: &AppHandle) {
    let _guard = record_access().lock().unwrap();
    if let Ok(entries) = std::fs::read_dir(terminal_dir()) {
        for entry in entries.flatten().take(10_000) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(Some(body)) = read_bounded_string(&path, 64 * 1024) {
                if let Ok(fact) = serde_json::from_str::<TerminalFact>(&body) {
                    if !fact.restored_orphan
                        && terminal_path(&fact.record_id).ok().as_ref() == Some(&path)
                    {
                        pending_terminals()
                            .lock()
                            .unwrap()
                            .entry(fact.record_id.clone())
                            .or_insert(fact);
                    }
                }
            }
        }
    }
    let pending: Vec<_> = pending_terminals()
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    for fact in pending {
        match persist_terminal(fact) {
            Ok(Some(completion)) => publish_automation_completion(app, completion),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "automation terminal persistence pending; will retry")
            }
        }
    }
}

fn claim_external_attempt(
    fact: &mut TerminalFact,
    push: bool,
    mut save: impl FnMut(&TerminalFact) -> Result<(), String>,
) -> Result<bool, String> {
    let should_send = push && !fact.external_claimed;
    if should_send {
        fact.external_claimed = true;
    }
    save(fact)?;
    Ok(should_send)
}

fn publish_pending_completion(app: &AppHandle, record_id: &str) {
    let completion = pending_terminals()
        .lock()
        .unwrap()
        .get(record_id)
        .and_then(|fact| fact.completion.clone());
    if let Some(completion) = completion {
        publish_automation_completion(app, completion);
    }
}

fn publications_in_flight() -> &'static Mutex<HashSet<String>> {
    static PUBLISHING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    PUBLISHING.get_or_init(|| Mutex::new(HashSet::new()))
}
fn publication_title(completion: &AutomationCompletion) -> String {
    format!(
        "自动化《{}》{}",
        completion.automation_name,
        if completion.event.status == "interrupted" {
            "已人工接管"
        } else if completion.event.status == "failed" {
            "运行失败"
        } else {
            "已完成"
        }
    )
}
fn finish_publication(record_id: &str, delivery: &str) -> Result<(), String> {
    let _guard = record_access().lock().unwrap();
    let mut fact = pending_terminals()
        .lock()
        .unwrap()
        .get(record_id)
        .cloned()
        .ok_or("完成事实不存在")?;
    fact.external_result = Some(delivery.into());
    write_terminal_fact(&fact)?;
    pending_terminals()
        .lock()
        .unwrap()
        .insert(record_id.into(), fact.clone());
    let mut records = read_records()?;
    if let Some(record) = records
        .records
        .iter_mut()
        .find(|record| record.id == record_id)
    {
        record.notification_delivery = Some(delivery.into());
        write_records(&mut records)?;
    }
    // Local inbox is append-once by record id. Removing the receipt only after
    // both publication stages makes a crash between append and cleanup safe.
    std::fs::remove_file(terminal_path(record_id)?).map_err(|e| e.to_string())?;
    pending_terminals().lock().unwrap().remove(record_id);
    Ok(())
}

pub(crate) fn publish_automation_completion(app: &AppHandle, completion: AutomationCompletion) {
    let record_id = completion.event.record_id.clone();
    if !publications_in_flight()
        .lock()
        .unwrap()
        .insert(record_id.clone())
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let prepared = (|| -> Result<(TerminalFact, bool), String> {
            let _guard = record_access().lock().unwrap();
            let mut fact = pending_terminals()
                .lock()
                .unwrap()
                .get(&record_id)
                .cloned()
                .ok_or("完成事实不存在")?;
            let failed = completion.event.status == "failed";
            let body = completion
                .event
                .error
                .as_deref()
                .unwrap_or("打开任务查看结果");
            let notification_id = crate::notifications::append_automation_once(
                &record_id,
                if failed {
                    crate::notifications::NotificationKind::Error
                } else {
                    crate::notifications::NotificationKind::SessionComplete
                },
                &publication_title(&completion),
                Some(body),
                completion.event.session_id.as_deref(),
                &completion.event.automation_id,
                if failed {
                    "error"
                } else if completion.event.status == "interrupted" {
                    "warn"
                } else {
                    "info"
                },
            )?;
            fact.local_notification_id = Some(notification_id);
            let send_external =
                claim_external_attempt(&mut fact, completion.push, write_terminal_fact)?;
            pending_terminals()
                .lock()
                .unwrap()
                .insert(record_id.clone(), fact.clone());
            Ok((fact, send_external))
        })();
        match prepared {
            Ok((fact, send_external)) => {
                emit_automation_update(&app, completion.event.clone());
                let delivery = if send_external {
                    let results = crate::notifications::dispatch_with_desktop_fallback(
                        &app,
                        crate::notifications::NotifyMessage {
                            title: publication_title(&completion),
                            body: Some(
                                completion
                                    .event
                                    .error
                                    .clone()
                                    .unwrap_or_else(|| "打开任务查看结果".into()),
                            ),
                            level: if completion.event.status == "failed" {
                                "error".into()
                            } else if completion.event.status == "interrupted" {
                                "warn".into()
                            } else {
                                "info".into()
                            },
                            session_id: completion.event.session_id.clone(),
                            automation_id: Some(completion.event.automation_id.clone()),
                            notification_id: fact.local_notification_id,
                            hidden_title: Some("自动化运行结果".into()),
                            ..Default::default()
                        },
                    )
                    .await;
                    if results.iter().all(|result| result.ok) {
                        "通知已投递".to_string()
                    } else {
                        "通知投递未全部成功，请查看通知渠道；不会自动重发已尝试的副作用".to_string()
                    }
                } else if let Some(result) = fact.external_result {
                    result
                } else if fact.external_claimed {
                    "外部通知结果未知：应用在投递期间中断，请核查渠道；系统不会盲目重发".into()
                } else {
                    "已保存到本地收件箱".into()
                };
                if let Err(error) = finish_publication(&record_id, &delivery) {
                    tracing::warn!(%error, %record_id, "automation publication receipt pending retry");
                }
            }
            Err(error) => {
                tracing::warn!(%error, %record_id, "automation local inbox pending retry")
            }
        }
        publications_in_flight().lock().unwrap().remove(&record_id);
    });
}
/// Whether permission requests for this automation session may be approved
/// without changing any interactive task's permission mode.
pub fn is_full_access_session(session_id: &str) -> bool {
    full_access_sessions().lock().unwrap().contains(session_id)
}

async fn release_full_access_session(
    app: &AppHandle,
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: &str,
) {
    let _guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;
    if !is_full_access_session(session_id) {
        return;
    }
    let target_mode = crate::permission_config::permission_mode_for_session();
    let mut reset = Ok(());
    for _ in 0..2 {
        reset = crate::permission_config::sync_runtime_session_permission_mode(
            tx,
            session_id,
            &target_mode,
        )
        .await;
        if reset.is_ok() {
            break;
        }
    }
    if let Err(error) = reset {
        tracing::warn!(%error, %session_id, "failed to restore automation session permission mode");
        let reason = format!(
            "自动化会话权限恢复失败（{error}）；为避免残留全权限会话，Agent 已安全停止，请重新启动"
        );
        let state = app.state::<AppState>();
        if state.mark_runtime_dead_if_current(tx, &reason) {
            let _ = app.emit(
                "agent://agent-died",
                serde_json::json!({ "reason": reason }),
            );
        }
    }
    full_access_sessions().lock().unwrap().remove(session_id);
}

pub fn clear_runtime_sessions() {
    full_access_sessions().lock().unwrap().clear();
}

fn fail_stale_running_records() {
    let _guard = record_access().lock().unwrap();
    let mut records = match read_records() {
        Ok(records) => records,
        Err(error) => {
            tracing::error!(%error, "failed to read automation records during startup recovery");
            return;
        }
    };
    let now = Local::now().to_rfc3339();
    let mut changed = false;
    for record in &mut records.records {
        if record.status == "running"
            && !pending_terminals().lock().unwrap().contains_key(&record.id)
        {
            record.status = "failed".into();
            record.finished_at = Some(now.clone());
            record.error = Some("运行结果未知：应用或 Agent 中断，未找到可恢复的完成事实；请核查会话，系统不会自动重跑".into());
            record.automation_snapshot = None;
            changed = true;
        }
    }
    if changed {
        if let Err(error) = write_records(&mut records) {
            tracing::error!(%error, "failed to persist stale automation recovery");
        } else {
            let pending = pending_terminals().lock().unwrap();
            active_run_prompts()
                .lock()
                .unwrap()
                .retain(|_, id| pending.contains_key(id));
        }
    }
}

// ---------- scheduling ----------

fn now_local() -> DateTime<Local> {
    Local::now()
}

fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.split(':');
    let h: u32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

fn at_local(date: NaiveDate, h: u32, m: u32) -> Option<DateTime<Local>> {
    date.and_time(NaiveTime::from_hms_opt(h, m, 0)?)
        .and_local_timezone(Local)
        .single()
}

fn weekday_code(date: NaiveDate) -> &'static str {
    match date.weekday() {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

/// Monday of the week containing `date` (week-parity anchor for 双周).
fn week_start(date: NaiveDate) -> NaiveDate {
    let offset = date.weekday().num_days_from_monday() as i64;
    date - Duration::days(offset)
}

fn recurring_next(
    sched: &AutomationSchedule,
    anchor: &DateTime<Local>,
    from: DateTime<Local>,
) -> Option<DateTime<Local>> {
    let h = sched.byhour.min(23);
    let m = sched.byminute.min(59);
    match sched.freq {
        ScheduleFreq::DAILY => {
            for i in 0..2 {
                let date = from.date_naive() + Duration::days(i);
                if let Some(candidate) = at_local(date, h, m) {
                    if candidate > from {
                        return Some(candidate);
                    }
                }
            }
            None
        }
        ScheduleFreq::WEEKLY => {
            let interval = sched.interval.clamp(1, 2) as i64;
            let anchor_week = week_start(anchor.date_naive());
            for i in 0..(7 * interval + 7) {
                let date = from.date_naive() + Duration::days(i);
                if !sched.byday.iter().any(|d| d == weekday_code(date)) {
                    continue;
                }
                if (week_start(date) - anchor_week).num_weeks() % interval != 0 {
                    continue;
                }
                if let Some(candidate) = at_local(date, h, m) {
                    if candidate > from {
                        return Some(candidate);
                    }
                }
            }
            None
        }
        ScheduleFreq::MONTHLY => {
            if sched.bymonthday.is_empty() {
                return None;
            }
            for i in 0..62 {
                let date = from.date_naive() + Duration::days(i);
                if !sched.bymonthday.contains(&date.day()) {
                    continue;
                }
                if let Some(candidate) = at_local(date, h, m) {
                    if candidate > from {
                        return Some(candidate);
                    }
                }
            }
            None
        }
        ScheduleFreq::YEARLY => {
            let month = *sched.bymonth.first()?;
            let day = *sched.bymonthday.first()?;
            // Search far enough to cover leap-day schedules. An invalid date in
            // one year (for example 02-29 in 2027) must not abort the search.
            for year_offset in 0..=8 {
                let year = from.year() + year_offset;
                let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
                    continue;
                };
                if let Some(candidate) = at_local(date, h, m) {
                    if candidate > from {
                        return Some(candidate);
                    }
                }
            }
            None
        }
        ScheduleFreq::HOURLY => {
            // 按间隔: fire at the configured start time + k*intervalHours on
            // selected weekdays. The old midnight anchor silently ignored the
            // time selected in the editor.
            let step = sched.interval_hours.clamp(1, 24);
            let byday = if sched.byday.is_empty() {
                ALL_DAYS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
            } else {
                sched.byday.clone()
            };
            for i in 0..8 {
                let date = from.date_naive() + Duration::days(i);
                if !byday.iter().any(|d| d == weekday_code(date)) {
                    continue;
                }
                let mut hour = h;
                while hour < 24 {
                    if let Some(candidate) = at_local(date, hour, m) {
                        if candidate > from {
                            return Some(candidate);
                        }
                    }
                    hour += step;
                }
            }
            None
        }
    }
}

/// Compute the next fire time for an automation after `from`, honoring the
/// validity window. Returns an RFC3339 string in local time.
fn compute_next_run(a: &Automation, from: DateTime<Local>) -> Option<String> {
    if a.status != "ACTIVE" {
        return None;
    }
    if a.schedule_type == "once" {
        let date = parse_date(a.scheduled_date.as_deref()?)?;
        let (h, m) = parse_hhmm(a.scheduled_time.as_deref().unwrap_or("09:00"))?;
        let candidate = at_local(date, h, m)?;
        // Fired or missed once-tasks have no upcoming run.
        return (candidate > from).then(|| candidate.to_rfc3339());
    }
    let valid_from = a.valid_from_date.as_deref().and_then(parse_date);
    let valid_until = a.valid_until_date.as_deref().and_then(parse_date);
    if let Some(until) = valid_until {
        if from.date_naive() > until {
            return None; // 已过期
        }
    }
    // Clamp the search start to the validity window's first day.
    let search_from = match valid_from {
        Some(fd) if from.date_naive() < fd => at_local(fd, 0, 0)? - Duration::minutes(1),
        _ => from,
    };
    let anchor = DateTime::parse_from_rfc3339(&a.created_at)
        .map(|dt| dt.with_timezone(&Local))
        .unwrap_or(from);
    let candidate = recurring_next(&a.schedule, &anchor, search_from)?;
    if let Some(until) = valid_until {
        if candidate.date_naive() > until {
            return None;
        }
    }
    Some(candidate.to_rfc3339())
}

/// Initialize only schedules that do not have a persisted next run. It is
/// essential not to recompute an already-due timestamp before the scheduler
/// gets a chance to claim it.
fn ensure_next_runs(store: &mut AutomationStore, now: DateTime<Local>) {
    for a in &mut store.automations {
        let next_run_missing_or_damaged = a
            .next_run_at
            .as_deref()
            .map(|value| DateTime::parse_from_rfc3339(value).is_err())
            .unwrap_or(true);
        if next_run_missing_or_damaged && a.status == "ACTIVE" {
            a.next_run_at = compute_next_run(a, now);
            // An ACTIVE schedule without any computable occurrence is a ghost:
            // it keeps the resident process/autostart alive but can never fire.
            // This also contains semantically damaged, but still valid JSON.
            if a.next_run_at.is_none() {
                a.status = "PAUSED".into();
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ClaimedAutomation {
    automation: Automation,
    scheduled_for: String,
}

fn scheduled_runs_in_flight(records: &RunRecordStore) -> HashSet<String> {
    records
        .records
        .iter()
        .filter(|record| {
            record.scheduled_for.is_some() && matches!(record.status.as_str(), "queued" | "running")
        })
        .map(|record| record.automation_id.clone())
        .collect()
}

/// Claim persisted due tasks using a fire-once misfire policy. Automations in
/// `blocked` already have a scheduled occurrence in flight and are deliberately
/// left due; the next scheduler tick coalesces the missed occurrence instead of
/// building an unbounded backlog. A manual test run does not block the first
/// due occurrence: it is persisted as queued immediately and serialized later.
fn claim_due_unblocked(
    store: &mut AutomationStore,
    now: DateTime<Local>,
    blocked: &HashSet<String>,
) -> Vec<ClaimedAutomation> {
    ensure_next_runs(store, now);
    let mut due = Vec::new();
    for a in &mut store.automations {
        if a.status != "ACTIVE" || blocked.contains(&a.id) {
            continue;
        }
        let scheduled_for = a
            .next_run_at
            .as_ref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .filter(|t| t.with_timezone(&Local) <= now)
            .map(|_| a.next_run_at.clone().unwrap_or_default());
        let Some(scheduled_for) = scheduled_for else {
            continue;
        };
        due.push(ClaimedAutomation {
            automation: a.clone(),
            scheduled_for,
        });
        a.last_run_at = Some(now.to_rfc3339());
        if a.schedule_type == "once" {
            a.status = "PAUSED".into();
            a.next_run_at = None;
        } else {
            a.next_run_at = compute_next_run(a, now);
            if a.next_run_at.is_none()
                && a.valid_until_date
                    .as_deref()
                    .and_then(parse_date)
                    .is_some_and(|until| until < now.date_naive())
            {
                a.status = "PAUSED".into();
            }
        }
    }
    due
}

#[cfg(test)]
fn claim_due(store: &mut AutomationStore, now: DateTime<Local>) -> Vec<Automation> {
    claim_due_unblocked(store, now, &HashSet::new())
        .into_iter()
        .map(|claim| claim.automation)
        .collect()
}

fn first_cwd(a: &Automation) -> Option<String> {
    a.cwds
        .split(',')
        .map(|c| c.trim())
        .find(|c| !c.is_empty())
        .map(|c| c.to_string())
}

fn normalize_authorized_cwds(
    filesystem: &crate::shell_fs::FilesystemAccess,
    raw: &str,
) -> Result<String, String> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for claimed in raw.split(',').map(str::trim).filter(|cwd| !cwd.is_empty()) {
        let canonical = filesystem.require_workspace(claimed)?;
        if seen.insert(canonical.clone()) {
            normalized.push(canonical.to_string_lossy().into_owned());
        }
    }
    Ok(normalized.join(","))
}

fn require_automation_cwd(
    filesystem: &crate::shell_fs::FilesystemAccess,
    automation: &Automation,
    default_cwd: &Path,
) -> Result<PathBuf, String> {
    let candidate = first_cwd(automation)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf());
    filesystem.require_workspace(&candidate.to_string_lossy())
}

fn validate_weekdays(days: &[String]) -> Result<(), String> {
    if days.is_empty() {
        return Err("请至少选择一个星期".into());
    }
    if days.iter().any(|d| !ALL_DAYS.contains(&d.as_str())) {
        return Err("执行星期包含无效值".into());
    }
    Ok(())
}

/// Validate the persisted contract at the backend boundary. Frontend
/// validation is useful feedback, but direct IPC calls and migrated files must
/// not be able to create an ACTIVE task that can never produce a next run.
fn validate_automation_at(a: &Automation, now: DateTime<Local>) -> Result<(), String> {
    selected_expert_identity(a)?;
    if a.name.trim().is_empty() {
        return Err("请填写自动化任务名称".into());
    }
    if a.prompt.trim().is_empty() {
        return Err("请填写提示词".into());
    }
    if !matches!(a.status.as_str(), "ACTIVE" | "PAUSED") {
        return Err("自动化任务状态无效".into());
    }
    if !matches!(a.permission_mode.as_str(), "default" | "fullAccess") {
        return Err("自动化权限模式无效".into());
    }
    if a.permission_mode == "fullAccess" {
        crate::permission_config::ensure_always_approve_available()?;
    }
    if !matches!(a.schedule_type.as_str(), "recurring" | "once") {
        return Err("自动化调度类型无效".into());
    }

    if a.schedule_type == "once" {
        let date = a
            .scheduled_date
            .as_deref()
            .and_then(parse_date)
            .ok_or("请选择有效的单次执行日期")?;
        let (hour, minute) = a
            .scheduled_time
            .as_deref()
            .and_then(parse_hhmm)
            .ok_or("请选择有效的单次执行时间")?;
        let candidate = at_local(date, hour, minute).ok_or("单次执行时间在当前时区无效")?;
        if a.status == "ACTIVE" && candidate <= now {
            return Err("单次执行时间必须晚于当前时间".into());
        }
        return Ok(());
    }

    if a.schedule.byhour > 23 || a.schedule.byminute > 59 {
        return Err("执行时间无效".into());
    }
    match a.schedule.freq {
        ScheduleFreq::DAILY => {}
        ScheduleFreq::WEEKLY => {
            validate_weekdays(&a.schedule.byday)?;
            if !(1..=2).contains(&a.schedule.interval) {
                return Err("每周执行间隔只能是 1 或 2 周".into());
            }
        }
        ScheduleFreq::MONTHLY => {
            if a.schedule.bymonthday.is_empty()
                || a.schedule.bymonthday.iter().any(|d| !(1..=31).contains(d))
            {
                return Err("请选择有效的每月执行日期".into());
            }
        }
        ScheduleFreq::YEARLY => {
            if a.schedule.bymonth.len() != 1
                || a.schedule.bymonthday.len() != 1
                || !(1..=12).contains(&a.schedule.bymonth[0])
                || !(1..=31).contains(&a.schedule.bymonthday[0])
                || !(2000..=2007).any(|year| {
                    NaiveDate::from_ymd_opt(year, a.schedule.bymonth[0], a.schedule.bymonthday[0])
                        .is_some()
                })
            {
                return Err("请选择有效的年度执行日期".into());
            }
        }
        ScheduleFreq::HOURLY => {
            validate_weekdays(&a.schedule.byday)?;
            if !(1..=24).contains(&a.schedule.interval_hours) {
                return Err("按间隔执行需设置为 1 至 24 小时".into());
            }
        }
    }

    let valid_from = match a.valid_from_date.as_deref() {
        Some(raw) => Some(parse_date(raw).ok_or("生效开始日期无效")?),
        None => None,
    };
    let valid_until = match a.valid_until_date.as_deref() {
        Some(raw) => Some(parse_date(raw).ok_or("生效结束日期无效")?),
        None => None,
    };
    if let (Some(from), Some(until)) = (valid_from, valid_until) {
        if from > until {
            return Err("生效开始日期不能晚于结束日期".into());
        }
    }
    if a.status == "ACTIVE" && valid_until.is_some_and(|until| until < now.date_naive()) {
        return Err("生效日期区间已过期，请更新结束日期".into());
    }
    if a.status == "ACTIVE" && compute_next_run(a, now).is_none() {
        return Err("当前调度和生效日期区间内没有可执行时间".into());
    }
    Ok(())
}

fn set_status_at(
    store: &mut AutomationStore,
    id: &str,
    status: &str,
    now: DateTime<Local>,
) -> Result<(), String> {
    let normalized_status = status.to_uppercase();
    let normalized = match normalized_status.as_str() {
        "ACTIVE" => "ACTIVE",
        "PAUSED" => "PAUSED",
        _ => return Err("自动化任务状态无效".into()),
    };
    let automation = store
        .automations
        .iter_mut()
        .find(|automation| automation.id == id)
        .ok_or_else(|| format!("automation {id} not found"))?;
    automation.status = normalized.into();
    if normalized == "ACTIVE" {
        validate_automation_at(automation, now)?;
    }
    // Recompute only the task whose status changed. Recomputing every task here
    // can move an unrelated, already-due timestamp into the future before the
    // scheduler has a chance to claim it, silently dropping that occurrence.
    automation.next_run_at = compute_next_run(automation, now);
    Ok(())
}

// ---------- Tauri commands ----------

/// Full snapshot: automations (with recomputed next runs) + run records.
#[tauri::command]
pub fn automations_snapshot(app: AppHandle) -> Result<AutomationSnapshot, String> {
    // Never hold both file-store locks: prompt completion acquires records
    // before looking up its automation, so nested locking here could deadlock.
    let automations = {
        let _guard = store_access().lock().unwrap();
        let mut store = read_store()?;
        ensure_next_runs(&mut store, now_local());
        write_store(&store)?;
        store.automations
    };
    let records = {
        let _guard = record_access().lock().unwrap();
        let mut records = read_records()?.records;
        let pending = pending_terminals().lock().unwrap();
        for record in &mut records {
            if pending.contains_key(&record.id)
                && matches!(record.status.as_str(), "queued" | "running")
            {
                record.completion_pending = true;
                record.error =
                    Some("运行已结束，完成记录等待保存；系统正在重试，不会重复执行任务".into());
            }
        }
        records
    };
    sync_autostart_enabled(
        &app,
        automations
            .iter()
            .any(|automation| automation.status == "ACTIVE"),
    );
    Ok(AutomationSnapshot {
        automations,
        records,
    })
}

/// Treat empty strings as absent for optional fields.
fn blank_to_none(value: &mut Option<String>) {
    if value.as_deref().map(str::trim) == Some("") {
        *value = None;
    }
}

/// Called under store_access: an edit must never recreate a deleted task.
fn save_automation_in_store(
    store: &mut AutomationStore,
    mut automation: Automation,
    creating: bool,
) -> Result<Automation, String> {
    if creating {
        store.automations.push(automation.clone());
    } else {
        let existing = store
            .automations
            .iter_mut()
            .find(|item| item.id == automation.id)
            .ok_or("原自动化任务已被移除，请保留草稿并新建任务。")?;
        // Scheduler metadata can advance while an editor is suspended.
        automation.created_at = existing.created_at.clone();
        automation.last_run_at = existing.last_run_at.clone();
        *existing = automation.clone();
    }
    Ok(automation)
}

#[tauri::command]
pub fn automations_save(
    app: AppHandle,
    state: State<'_, AppState>,
    mut automation: Automation,
) -> Result<Automation, String> {
    crate::policy::require_feature("automations")?;
    // Normalize before validation so an empty renderer field has exactly the
    // same Auto semantics as an omitted field.
    blank_to_none(&mut automation.model_id);
    if let Some(model_id) = automation.model_id.as_deref() {
        crate::policy::require_model(model_id)?;
    }
    // Saving an ACTIVE-looking task that can never run is a product bug. Auto
    // and fixed selections must both resolve against the live Runtime now; the
    // same check is repeated at dispatch because configuration can later change.
    // Paused tasks remain editable while offline and are validated on resume/run.
    if automation.status.eq_ignore_ascii_case("ACTIVE") {
        crate::commands::resolve_automation_model_id(
            Some(&app),
            &state,
            automation.model_id.as_deref(),
        )?;
    }
    let _guard = store_access().lock().unwrap();
    let mut store = read_store()?;
    let creating = automation.id.is_empty();
    let id = if creating {
        uuid::Uuid::now_v7().to_string()
    } else {
        automation.id.clone()
    };
    let now = now_local().to_rfc3339();
    let mut final_automation = automation;
    final_automation.id = id.clone();
    if final_automation.created_at.is_empty() {
        final_automation.created_at = now;
    }
    final_automation.status = final_automation.status.to_uppercase();
    // Frontend sends "" for unset optionals; normalize to None.
    blank_to_none(&mut final_automation.expert_id);
    blank_to_none(&mut final_automation.expert_name);
    blank_to_none(&mut final_automation.expert_path);
    blank_to_none(&mut final_automation.scheduled_date);
    blank_to_none(&mut final_automation.scheduled_time);
    blank_to_none(&mut final_automation.valid_from_date);
    blank_to_none(&mut final_automation.valid_until_date);
    final_automation.name = final_automation.name.trim().to_string();
    final_automation.prompt = final_automation.prompt.trim().to_string();
    final_automation.cwds = normalize_authorized_cwds(
        &app.state::<crate::shell_fs::FilesystemAccess>(),
        &final_automation.cwds,
    )?;
    final_automation.skills.sort();
    final_automation.skills.dedup();
    final_automation.connector_ids.sort();
    final_automation.connector_ids.dedup();
    validate_automation_at(&final_automation, now_local())?;
    final_automation.next_run_at = compute_next_run(&final_automation, now_local());

    let final_automation = save_automation_in_store(&mut store, final_automation, creating)?;
    write_store(&store)?;
    let has_active = store
        .automations
        .iter()
        .any(|automation| automation.status == "ACTIVE");
    drop(_guard);
    sync_autostart_enabled(&app, has_active);
    Ok(final_automation)
}

#[tauri::command]
pub fn automations_delete(app: AppHandle, id: String) -> Result<(), String> {
    crate::policy::require_feature("automations")?;
    let _record_guard = record_access().lock().unwrap();
    if read_records()?.records.iter().any(|record| {
        record.automation_id == id && matches!(record.status.as_str(), "queued" | "running")
    }) {
        return Err("该自动化任务正在运行，请等待完成后再删除".into());
    }
    let _store_guard = store_access().lock().unwrap();
    let mut store = read_store()?;
    if !store
        .automations
        .iter()
        .any(|automation| automation.id == id)
    {
        return Err(format!("automation {id} not found"));
    }
    store.automations.retain(|a| a.id != id);
    write_store(&store)?;
    let has_active = store
        .automations
        .iter()
        .any(|automation| automation.status == "ACTIVE");
    drop(_store_guard);
    drop(_record_guard);
    sync_autostart_enabled(&app, has_active);
    Ok(())
}

#[tauri::command]
pub fn automations_set_status(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    status: String,
) -> Result<(), String> {
    crate::policy::require_feature("automations")?;
    let _guard = store_access().lock().unwrap();
    let mut store = read_store()?;
    if status.eq_ignore_ascii_case("ACTIVE") {
        let automation = store
            .automations
            .iter()
            .find(|automation| automation.id == id)
            .ok_or_else(|| format!("automation {id} not found"))?;
        crate::commands::resolve_automation_model_id(
            Some(&app),
            &state,
            automation.model_id.as_deref(),
        )?;
    }
    set_status_at(&mut store, &id, &status, now_local())?;
    write_store(&store)?;
    let has_active = store
        .automations
        .iter()
        .any(|automation| automation.status == "ACTIVE");
    drop(_guard);
    sync_autostart_enabled(&app, has_active);
    Ok(())
}

/// Manually fire an automation now (test run). Opens a new EchoAgent session and
/// sends the prompt — the result appears in the sidebar like any chat, and a
/// run record is written for the 运行记录 tab.
#[tauri::command]
pub async fn automations_run(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<String, String> {
    crate::policy::require_feature("automations")?;
    let automation = {
        let _guard = store_access().lock().unwrap();
        read_store()?
            .automations
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("automation {id} not found"))?
    };
    let default_cwd = PathBuf::from(first_cwd(&automation).unwrap_or_else(|| {
        state
            .cwd
            .lock()
            .unwrap()
            .clone()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }));
    let cwd = require_automation_cwd(
        &app.state::<crate::shell_fs::FilesystemAccess>(),
        &automation,
        &default_cwd,
    )?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let resolved_model_id = crate::commands::resolve_automation_model_id(
        Some(&app),
        &state,
        automation.model_id.as_deref(),
    )?;

    let started = now_local().to_rfc3339();
    let record_id = record_run_started(&automation, &started, &cwd, None, &resolved_model_id)?;

    // Mark last-run.
    let update_result = (|| {
        let _guard = store_access().lock().unwrap();
        let mut store = read_store()?;
        let current = store
            .automations
            .iter_mut()
            .find(|current| current.id == id)
            .ok_or_else(|| format!("automation {id} not found"))?;
        current.last_run_at = Some(started.clone());
        write_store(&store)
    })();
    if let Err(error) = update_result {
        if record_run_finished(&record_id, false, None, Some(&error)) {
            emit_run_update(
                &app,
                &automation,
                &record_id,
                &cwd,
                RunUpdate::Failed {
                    session_id: None,
                    error: &error,
                },
            );
        }
        return Err(error);
    }
    emit_run_update(&app, &automation, &record_id, &cwd, RunUpdate::Queued);
    let task_record_id = record_id.clone();
    tauri::async_runtime::spawn(execute_automation_run(
        app,
        tx,
        automation,
        cwd,
        task_record_id,
    ));
    // The command acknowledges durable dispatch, not completion. The run record
    // and notification stream are the authoritative lifecycle surface.
    Ok(record_id)
}

async fn notify_run_failure(app: &AppHandle, automation: &Automation, error: &str) {
    tracing::warn!(automation_id = %automation.id, %error, "automation run failed");
    let notification_id = crate::notifications::append_with_target(
        crate::notifications::NotificationKind::Error,
        &format!("自动化《{}》运行失败", automation.name),
        Some(error),
        None,
        None,
        Some(&automation.id),
        "error",
    )
    .ok();
    if automation.push_to_we_chat {
        let _ = crate::notifications::dispatch_with_desktop_fallback(
            app,
            crate::notifications::NotifyMessage {
                title: format!("自动化失败：{}", automation.name),
                body: Some("打开自动化查看运行记录".into()),
                level: "error".into(),
                session_id: None,
                automation_id: Some(automation.id.clone()),
                notification_id,
                hidden_title: Some("自动化运行失败".into()),
                ..Default::default()
            },
        )
        .await;
    }
}

async fn execute_automation_run(
    app: AppHandle,
    tx: echo_agent_acp::AcpAgentTx,
    automation: Automation,
    cwd: PathBuf,
    record_id: String,
) {
    let _automation_guard = automation_run_lock(&automation.id).lock_owned().await;
    let _slot = match automation_run_slots().clone().acquire_owned().await {
        Ok(slot) => slot,
        Err(error) => {
            let error = format!("自动化执行队列已关闭：{error}");
            if record_run_finished(&record_id, false, None, Some(&error)) {
                emit_run_update(
                    &app,
                    &automation,
                    &record_id,
                    &cwd,
                    RunUpdate::Failed {
                        session_id: None,
                        error: &error,
                    },
                );
                publish_pending_completion(&app, &record_id);
            }
            return;
        }
    };
    // Re-resolve immediately before dispatch. A queued record is durable data,
    // not a capability, and must not regain authority merely by surviving a
    // restart or by containing an attacker-edited cwd.
    let cwd = match app
        .state::<crate::shell_fs::FilesystemAccess>()
        .require_workspace(&cwd.to_string_lossy())
    {
        Ok(cwd) => cwd,
        Err(error) => {
            let error = format!("自动化工作区授权校验失败：{error}");
            if record_run_finished(&record_id, false, None, Some(&error)) {
                emit_run_update(
                    &app,
                    &automation,
                    &record_id,
                    &cwd,
                    RunUpdate::Failed {
                        session_id: None,
                        error: &error,
                    },
                );
                publish_pending_completion(&app, &record_id);
            }
            return;
        }
    };
    // Scheduler tasks are detached from the scheduler loop itself. If the Agent
    // Runtime was replaced while this dispatch waited for a concurrency slot,
    // leave the durable row queued for the replacement scheduler to recover;
    // never consume it through a retired channel.
    let runtime_is_current = app
        .state::<AppState>()
        .tx
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|current| current.same_channel(&tx));
    if !runtime_is_current {
        tracing::info!(
            automation_id = %automation.id,
            %record_id,
            "leaving queued automation for the replacement runtime"
        );
        return;
    }
    let resolved_model_id = match crate::commands::resolve_automation_model_id(
        Some(&app),
        &app.state::<AppState>(),
        automation.model_id.as_deref(),
    ) {
        Ok(model_id) => model_id,
        Err(error) => {
            if record_run_finished(&record_id, false, None, Some(&error)) {
                emit_run_update(
                    &app,
                    &automation,
                    &record_id,
                    &cwd,
                    RunUpdate::Failed {
                        session_id: None,
                        error: &error,
                    },
                );
                publish_pending_completion(&app, &record_id);
            }
            return;
        }
    };
    loop {
        match mark_record_running(&record_id, &resolved_model_id) {
            Ok(()) => {
                emit_run_update(&app, &automation, &record_id, &cwd, RunUpdate::Running);
                break;
            }
            Err(error) if error.contains("not dispatchable") || error.contains("not found") => {
                tracing::warn!(
                    %error,
                    automation_id = %automation.id,
                    %record_id,
                    "automation dispatch is no longer eligible"
                );
                return;
            }
            Err(error) => {
                // A transient disk error must not strand a durable queued row
                // forever. Keep it queued and retry; a replacement runtime can
                // still take over because the current-channel check is repeated.
                tracing::warn!(
                    %error,
                    automation_id = %automation.id,
                    %record_id,
                    "automation dispatch could not be persisted as running; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let runtime_is_current = app
                    .state::<AppState>()
                    .tx
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|current| current.same_channel(&tx));
                if !runtime_is_current {
                    return;
                }
            }
        }
    }

    let result = {
        let state = app.state::<AppState>();
        tokio::time::timeout(
            AUTOMATION_RUN_TIMEOUT,
            run_automation_once(
                &app,
                &state,
                &tx,
                &automation,
                &cwd,
                &record_id,
                &resolved_model_id,
            ),
        )
        .await
    };

    match result {
        Ok(Ok((session_id, completion))) => {
            crate::bridge::handle_prompt_complete(&app, completion).await;
            release_full_access_session(&app, &tx, &session_id).await;
        }
        Ok(Err(error)) => {
            match record_session_id(&record_id) {
                Ok(Some(session_id)) => {
                    release_full_access_session(&app, &tx, &session_id).await;
                    app.state::<AppState>()
                        .forget_session_workspace(&session_id);
                }
                Ok(None) => {}
                Err(read_error) => {
                    tracing::error!(error = %read_error, %record_id, "failed to read failed automation session");
                }
            }
            let session_id = record_session_id(&record_id).ok().flatten();
            if record_run_finished(&record_id, false, None, Some(&error)) {
                emit_run_update(
                    &app,
                    &automation,
                    &record_id,
                    &cwd,
                    RunUpdate::Failed {
                        session_id: session_id.as_deref(),
                        error: &error,
                    },
                );
                publish_pending_completion(&app, &record_id);
            }
        }
        Err(_) => {
            let error = format!(
                "执行超过 {} 分钟，已取消",
                AUTOMATION_RUN_TIMEOUT.as_secs() / 60
            );
            match record_session_id(&record_id) {
                Ok(Some(session_id)) => {
                    let _ = crate::agent_runtime::cancel(&tx, &session_id, "stop", None).await;
                    release_full_access_session(&app, &tx, &session_id).await;
                    app.state::<AppState>()
                        .forget_session_workspace(&session_id);
                }
                Ok(None) => {}
                Err(read_error) => {
                    tracing::error!(error = %read_error, %record_id, "failed to read timed-out automation session");
                }
            }
            let session_id = record_session_id(&record_id).ok().flatten();
            if record_run_finished(&record_id, false, None, Some(&error)) {
                emit_run_update(
                    &app,
                    &automation,
                    &record_id,
                    &cwd,
                    RunUpdate::Failed {
                        session_id: session_id.as_deref(),
                        error: &error,
                    },
                );
                publish_pending_completion(&app, &record_id);
            }
        }
    }
}

/// A Runtime session already exists when desktop metadata is committed. Every
/// failed commit must revoke its launch-only authority and stop the session,
/// even when no durable run record yet knows the newly allocated session id.
#[derive(Debug)]
struct CreatedAutomationSessionFailure {
    error: String,
    permission_reset_error: Option<String>,
}

async fn commit_created_automation_session(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: &str,
    full_access: bool,
    target_mode: &str,
    bind_workspace: impl FnOnce() -> Result<(), String>,
    record_run: impl FnOnce() -> Result<(), String>,
    quarantine: impl FnOnce(&str),
) -> Result<(), CreatedAutomationSessionFailure> {
    let Err(error) = bind_workspace().and_then(|()| record_run()) else {
        return Ok(());
    };
    let mut permission_reset_error = None;
    if full_access {
        for _ in 0..2 {
            match crate::permission_config::sync_runtime_session_permission_mode(
                tx,
                session_id,
                target_mode,
            )
            .await
            {
                Ok(()) => {
                    permission_reset_error = None;
                    break;
                }
                Err(error) => permission_reset_error = Some(error),
            }
        }
    }
    // Preserve fail-closed ordering: quarantine immediately after failed reset,
    // before waiting for an unresponsive actor's cancellation acknowledgement.
    if let Some(error) = &permission_reset_error {
        quarantine(error);
    }
    let error = match crate::agent_runtime::cancel(tx, session_id, "stop", None).await {
        Ok(()) => error,
        Err(cancel_error) => format!("{error}；停止未完成的自动化会话失败：{cancel_error}"),
    };
    Err(CreatedAutomationSessionFailure {
        error,
        permission_reset_error,
    })
}

/// Open a fresh EchoAgent session and send the automation prompt.
/// Returns the new session id on success.
async fn run_automation_once(
    app: &AppHandle,
    state: &AppState,
    tx: &echo_agent_acp::AcpAgentTx,
    automation: &Automation,
    cwd: &Path,
    record_id: &str,
    resolved_model_id: &str,
) -> Result<(String, crate::bridge::CompleteEvent), String> {
    crate::policy::require_feature("automations")?;
    crate::policy::require_model(resolved_model_id)?;
    crate::commands::require_runtime_ready(Some(app), state, Some(resolved_model_id))?;
    if !cwd.is_dir() {
        return Err(format!("自动化工作空间不存在：{}", cwd.display()));
    }
    let context = resolve_execution_context(tx, automation, cwd).await?;
    let reasoning_effort = automation.model_is_thinking.then_some("high");
    let full_access = automation.permission_mode == "fullAccess";
    let permission_guard = if full_access {
        crate::permission_config::ensure_always_approve_available()?;
        Some(
            crate::permission_config::permission_transition_lock()
                .lock()
                .await,
        )
    } else {
        None
    };
    let session_id = crate::agent_runtime::new_session_with_options(
        tx,
        cwd,
        Some(resolved_model_id),
        reasoning_effort,
        full_access.then_some("always-approve"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let prompt_id = uuid::Uuid::now_v7().to_string();
    let target_mode = crate::permission_config::permission_mode_for_session();
    if let Err(failure) = commit_created_automation_session(
        tx,
        &session_id,
        full_access,
        &target_mode,
        || state.record_session_workspace(&session_id, cwd),
        || record_run_session(record_id, &session_id, &prompt_id),
        |reset_error| {
            let reason = format!(
                "自动化会话创建失败后无法恢复权限（{reset_error}）；为避免残留全权限会话，Agent 已安全停止，请重新启动"
            );
            if state.mark_runtime_dead_if_current(tx, &reason) {
                let _ = app.emit(
                    "agent://agent-died",
                    serde_json::json!({ "reason": reason }),
                );
            }
        },
    )
    .await
    {
        if let Some(reset_error) = failure.permission_reset_error {
            tracing::error!(session_id, error = %reset_error, "automation commit cleanup quarantined Runtime");
        }
        state.forget_session_workspace(&session_id);
        return Err(failure.error);
    }
    emit_run_update(
        app,
        automation,
        record_id,
        cwd,
        RunUpdate::SessionCreated(&session_id, resolved_model_id),
    );
    if full_access {
        full_access_sessions()
            .lock()
            .unwrap()
            .insert(session_id.clone());
    }
    drop(permission_guard);
    if let (Some(expert_id), Some(expert_name)) = (&automation.expert_id, &automation.expert_name) {
        let _ = crate::meta::set_expert(
            &session_id,
            crate::meta::ExpertBinding {
                expert_id: expert_id.clone(),
                expert_name: expert_name.clone(),
                source: "automation".into(),
                avatar_local: None,
            },
        );
    }
    let prompt = automation_prompt(automation, &context);
    let completion =
        crate::agent_runtime::prompt_for_automation(tx, &session_id, &prompt_id, &prompt)
            .await
            .map_err(|e| {
                state.forget_session_workspace(&session_id);
                e.to_string()
            })?;
    Ok((session_id, completion))
}

#[derive(Debug, Default)]
struct AutomationExecutionContext {
    expert_prompt: Option<String>,
    skills: Vec<(String, String, String)>,
    connectors: Vec<String>,
}

fn require_automated_skill_capability(
    capability: Option<
        &echo_agent_tools::implementations::skills::capability::SkillCapabilityReport,
    >,
    skill_name: &str,
) -> Result<(), String> {
    use echo_agent_tools::implementations::skills::capability::SkillCapabilityState;

    let capability = capability
        .ok_or_else(|| format!("所选技能未完成执行能力预检，无法启动无人值守任务：{skill_name}"))?;
    if capability.state.can_run_automated() {
        return Ok(());
    }
    let reason = match capability.state {
        SkillCapabilityState::MissingDependencies => "缺少运行依赖",
        SkillCapabilityState::ConfigurationRequired => "尚未完成账号连接或系统授权",
        SkillCapabilityState::Invalid => "执行能力清单无效",
        SkillCapabilityState::InstructionOnly | SkillCapabilityState::Ready => {
            unreachable!("runnable capability state passed the fail-closed guard")
        }
    };
    Err(format!(
        "所选技能{reason}，无法启动无人值守任务：{skill_name}"
    ))
}

fn selected_expert_identity(automation: &Automation) -> Result<Option<(&str, &str)>, String> {
    let name = automation
        .expert_name
        .as_deref()
        .or(automation.expert_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let path = automation
        .expert_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (name, path) {
        (None, None) => Ok(None),
        (Some(name), Some(path)) => Ok(Some((name, path))),
        _ => Err(
            "专家引用缺少稳定定义路径或名称，请打开自动化编辑器重新选择专家后保存；未派发任务"
                .into(),
        ),
    }
}

fn resolve_selected_expert_prompt(
    automation: &Automation,
    cwd: &Path,
) -> Result<Option<String>, String> {
    let Some((name, path)) = selected_expert_identity(automation)? else {
        return Ok(None);
    };
    let path = Path::new(path);
    crate::agents_store::require_runtime_identity(name, path, cwd)
        .map_err(|error| error.replace("请在团队中", "请在自动化编辑器中"))?;
    // Capture the effective definition and verify its identity again. The
    // prompt below comes from this checked snapshot, never a later name-only lookup.
    let definition = echo_agent_core::discovery::by_name_in_cwd(name, cwd)
        .ok_or_else(|| format!("所选专家已不可用：{name}，请重新选择"))?;
    let expected = path
        .canonicalize()
        .map_err(|_| "专家定义已不存在，请重新选择".to_string())?;
    if definition
        .source_path
        .as_ref()
        .and_then(|source| source.canonicalize().ok())
        .as_deref()
        != Some(expected.as_path())
    {
        return Err(format!(
            "专家“{name}”的生效定义已变化，请重新选择；未派发任务"
        ));
    }
    definition
        .prompt_body
        .filter(|body| !body.trim().is_empty())
        .map(Some)
        .ok_or_else(|| format!("所选专家没有有效角色定义：{name}，请重新选择"))
}
async fn resolve_execution_context(
    tx: &echo_agent_acp::AcpAgentTx,
    automation: &Automation,
    cwd: &Path,
) -> Result<AutomationExecutionContext, String> {
    let mut context = AutomationExecutionContext {
        expert_prompt: resolve_selected_expert_prompt(automation, cwd)?,
        ..Default::default()
    };

    if !automation.skills.is_empty() {
        let catalog =
            crate::skills::skills_list_with_tx(tx, Some(cwd.to_string_lossy().into_owned()))
                .await?;
        for selected in &automation.skills {
            let skill = catalog
                .iter()
                .find(|skill| skill.name == *selected)
                .ok_or_else(|| format!("所选技能不存在：{selected}"))?;
            if !skill.enabled {
                return Err(format!("所选技能未启用：{selected}"));
            }
            require_automated_skill_capability(skill.capability.as_ref(), selected)?;
            let configured_path = skill
                .path
                .as_deref()
                .ok_or_else(|| format!("所选技能缺少本地指令路径：{selected}"))?;
            let mut skill_path = PathBuf::from(configured_path);
            if skill_path.is_dir() {
                skill_path = skill_path.join("SKILL.md");
            }
            let raw = read_bounded_string(&skill_path, MAX_AUTOMATION_SKILL_BYTES)?
                .ok_or_else(|| format!("所选技能的指令文件不存在：{selected}"))?;
            let body = crate::agents_store::markdown_body(&raw);
            if body.is_empty() {
                return Err(format!("所选技能没有可执行指令：{selected}"));
            }
            let body =
                echo_agent_tools::implementations::skills::capability::append_runtime_contract(
                    body,
                    &skill_path,
                )
                .map_err(|error| format!("所选技能的执行能力清单无效：{selected}：{error}"))?;
            context.skills.push((
                selected.clone(),
                skill_path.to_string_lossy().into_owned(),
                body,
            ));
        }
    }

    if !automation.connector_ids.is_empty() {
        let connectors = crate::mcp::mcp_list_with_tx(tx, None).await?;
        for selected in &automation.connector_ids {
            let connector = connectors
                .iter()
                .find(|connector| connector.name == *selected)
                .ok_or_else(|| format!("所选连接器未配置：{selected}"))?;
            if !connector.enabled {
                return Err(format!("所选连接器未启用：{selected}"));
            }
            context.connectors.push(connector.name.clone());
        }
    }

    Ok(context)
}

fn automation_prompt(automation: &Automation, resolved: &AutomationExecutionContext) -> String {
    let mut context = Vec::new();
    if let Some(expert_prompt) = &resolved.expert_prompt {
        let expert_name = automation.expert_name.as_deref().unwrap_or("所选专家");
        context.push(format!(
            "<automation-expert name=\"{expert_name}\">\n{expert_prompt}\n</automation-expert>\n必须严格按照以上专家角色定义执行。"
        ));
    }
    for (name, path, body) in &resolved.skills {
        context.push(format!(
            "<automation-skill name=\"{name}\" path=\"{path}\">\n{body}\n</automation-skill>\n这是用户明确选择的技能，必须遵循其中的执行流程。"
        ));
    }
    if !resolved.connectors.is_empty() {
        context.push(format!(
            "以下 MCP 连接器已经过运行前检查并处于启用状态；需要外部能力时优先使用：{}。",
            resolved.connectors.join("、")
        ));
    }
    if context.is_empty() {
        automation.prompt.clone()
    } else {
        format!(
            "[自动化执行上下文]\n{}\n\n[自动化任务]\n{}",
            context.join("\n"),
            automation.prompt
        )
    }
}

#[tauri::command]
pub fn automation_records_archive(id: String, archived: bool) -> Result<(), String> {
    crate::policy::require_feature("automations")?;
    let _guard = record_access().lock().unwrap();
    let mut records = read_records()?;
    let record = records
        .records
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("automation record {id} not found"))?;
    if matches!(record.status.as_str(), "queued" | "running") {
        return Err("运行中的记录不能归档".into());
    }
    record.archived = archived;
    write_records(&mut records)
}

#[tauri::command]
pub fn automation_records_delete(id: String) -> Result<(), String> {
    crate::policy::require_feature("automations")?;
    let _guard = record_access().lock().unwrap();
    let mut records = read_records()?;
    let record = records
        .records
        .iter()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("automation record {id} not found"))?;
    if matches!(record.status.as_str(), "queued" | "running") {
        return Err("运行中的记录不能删除".into());
    }
    records.records.retain(|r| r.id != id);
    write_records(&mut records)
}

// ---------- background scheduler ----------

#[derive(Debug)]
struct AutomationDispatch {
    automation: Automation,
    cwd: PathBuf,
    record_id: String,
}

#[derive(Debug)]
struct RejectedAutomationDispatch {
    automation: Automation,
    cwd: PathBuf,
    record_id: String,
    error: String,
}

fn advance_recovered_occurrence(
    automation: &mut Automation,
    scheduled_for: &str,
    started_at: &str,
    now: &DateTime<Local>,
) -> bool {
    if automation.next_run_at.as_deref() != Some(scheduled_for) {
        return false;
    }
    automation.last_run_at = Some(started_at.to_string());
    if automation.schedule_type == "once" {
        automation.status = "PAUSED".into();
        automation.next_run_at = None;
    } else {
        automation.next_run_at = compute_next_run(automation, *now);
        if automation.next_run_at.is_none()
            && automation
                .valid_until_date
                .as_deref()
                .and_then(parse_date)
                .is_some_and(|until| until < now.date_naive())
        {
            automation.status = "PAUSED".into();
        }
    }
    true
}

/// Recreate dispatches that were durably queued before an application crash.
/// For a crash between writing the queue row and advancing the schedule, also
/// reconcile that exact occurrence so it cannot be fired twice.
fn recover_queued_dispatches(
    default_cwd: &Path,
    filesystem: &crate::shell_fs::FilesystemAccess,
) -> Result<Vec<AutomationDispatch>, String> {
    let _record_guard = record_access().lock().unwrap();
    let _store_guard = store_access().lock().unwrap();
    let mut records = read_records()?;
    let mut store = read_store()?;
    let now = now_local();
    let mut records_changed = false;
    let mut store_changed = false;
    let mut dispatches = Vec::new();

    for record in records
        .records
        .iter_mut()
        .filter(|record| record.status == "queued")
    {
        let Some(automation) = store
            .automations
            .iter_mut()
            .find(|automation| automation.id == record.automation_id)
        else {
            record.status = "failed".into();
            record.finished_at = Some(now.to_rfc3339());
            record.error = Some("自动化任务已删除，无法恢复未完成的运行".into());
            record.automation_snapshot = None;
            records_changed = true;
            continue;
        };

        if let Some(scheduled_for) = record.scheduled_for.as_deref() {
            store_changed |=
                advance_recovered_occurrence(automation, scheduled_for, &record.started_at, &now);
        }
        let claimed_cwd = record
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| first_cwd(automation).map(PathBuf::from))
            .unwrap_or_else(|| default_cwd.to_path_buf());
        let cwd = match filesystem.require_workspace(&claimed_cwd.to_string_lossy()) {
            Ok(cwd) => cwd,
            Err(error) => {
                record.status = "failed".into();
                record.finished_at = Some(now.to_rfc3339());
                record.error = Some(format!("恢复任务的工作区未授权：{error}"));
                record.automation_snapshot = None;
                records_changed = true;
                continue;
            }
        };
        dispatches.push(AutomationDispatch {
            automation: record
                .automation_snapshot
                .clone()
                .unwrap_or_else(|| automation.clone()),
            cwd,
            record_id: record.id.clone(),
        });
    }

    if store_changed {
        if let Err(error) = write_store(&store) {
            tracing::error!(%error, "failed to reconcile recovered automation schedule");
            let failed_at = now.to_rfc3339();
            let ids: HashSet<&str> = dispatches
                .iter()
                .map(|dispatch| dispatch.record_id.as_str())
                .collect();
            for record in &mut records.records {
                if ids.contains(record.id.as_str()) {
                    record.status = "failed".into();
                    record.finished_at = Some(failed_at.clone());
                    record.error = Some(format!("恢复自动化调度状态失败：{error}"));
                    record.automation_snapshot = None;
                    records_changed = true;
                }
            }
            dispatches.clear();
        }
    }
    if records_changed {
        if let Err(error) = write_records(&mut records) {
            tracing::error!(%error, "failed to persist recovered automation records");
            return Err(error);
        }
    }
    Ok(dispatches)
}

/// Scheduler tick. Fires any automation whose `next_run_at` has passed.
pub async fn scheduler_tick(app: &AppHandle, tx: &echo_agent_acp::AcpAgentTx, default_cwd: &Path) {
    let state = app.state::<AppState>();
    if let Err(error) = crate::commands::require_runtime_ready(Some(app), &state, None) {
        tracing::debug!(%error, "automation scheduler paused while Agent Runtime is not ready");
        return;
    }
    let now = now_local();
    let (dispatches, rejected_dispatches, active_state_changed, has_active) = {
        // Always acquire records before the automation store. Keeping one lock
        // order prevents a completion and a scheduler tick from deadlocking.
        let _record_guard = record_access().lock().unwrap();
        let mut records = match read_records() {
            Ok(records) => records,
            Err(error) => {
                tracing::error!(%error, "automation scheduler could not read run records");
                return;
            }
        };
        let blocked = scheduled_runs_in_flight(&records);
        let _store_guard = store_access().lock().unwrap();
        let mut store = match read_store() {
            Ok(store) => store,
            Err(error) => {
                tracing::error!(%error, "automation scheduler could not read schedules");
                return;
            }
        };
        let had_active = store
            .automations
            .iter()
            .any(|automation| automation.status == "ACTIVE");
        let claimed = claim_due_unblocked(&mut store, now, &blocked);
        let had_claims = !claimed.is_empty();
        let started = now.to_rfc3339();
        let mut dispatches = Vec::with_capacity(claimed.len());
        let mut rejected_dispatches = Vec::new();
        for claim in claimed {
            let claimed_cwd = first_cwd(&claim.automation)
                .map(PathBuf::from)
                .unwrap_or_else(|| default_cwd.to_path_buf());
            let resolved_model_id = crate::commands::resolve_automation_model_id(
                Some(app),
                &state,
                claim.automation.model_id.as_deref(),
            );
            let record_id = append_run_started(
                &mut records,
                &claim.automation,
                &started,
                &claimed_cwd,
                Some(&claim.scheduled_for),
                resolved_model_id.as_deref().ok(),
            );
            if let Err(error) = resolved_model_id {
                if let Some(record) = records
                    .records
                    .iter_mut()
                    .find(|record| record.id == record_id)
                {
                    finalize_record(record, false, &started, None, Some(&error));
                }
                rejected_dispatches.push(RejectedAutomationDispatch {
                    automation: claim.automation,
                    cwd: claimed_cwd,
                    record_id,
                    error,
                });
                continue;
            }
            let cwd = match app
                .state::<crate::shell_fs::FilesystemAccess>()
                .require_workspace(&claimed_cwd.to_string_lossy())
            {
                Ok(cwd) => cwd,
                Err(error) => {
                    let error = format!("自动化工作区未授权：{error}");
                    if let Some(record) = records
                        .records
                        .iter_mut()
                        .find(|record| record.id == record_id)
                    {
                        finalize_record(record, false, &started, None, Some(&error));
                    }
                    rejected_dispatches.push(RejectedAutomationDispatch {
                        automation: claim.automation,
                        cwd: claimed_cwd,
                        record_id,
                        error,
                    });
                    continue;
                }
            };
            dispatches.push(AutomationDispatch {
                automation: claim.automation,
                cwd,
                record_id,
            });
        }
        let has_active = store
            .automations
            .iter()
            .any(|automation| automation.status == "ACTIVE");
        if had_claims {
            if let Err(error) = write_records(&mut records) {
                tracing::error!(%error, "failed to persist queued automation runs");
                return;
            }
        }
        if let Err(error) = write_store(&store) {
            let failed_at = now.to_rfc3339();
            let ids: HashSet<&str> = dispatches
                .iter()
                .map(|dispatch| dispatch.record_id.as_str())
                .collect();
            for record in &mut records.records {
                if ids.contains(record.id.as_str()) {
                    record.status = "failed".into();
                    record.finished_at = Some(failed_at.clone());
                    record.error = Some(format!("保存下次调度时间失败：{error}"));
                    record.automation_snapshot = None;
                }
            }
            if let Err(record_error) = write_records(&mut records) {
                tracing::error!(error = %record_error, "failed to persist rejected automation dispatches");
            }
            tracing::error!(%error, "failed to persist automation schedule claim");
            return;
        }
        (
            dispatches,
            rejected_dispatches,
            had_active != has_active,
            has_active,
        )
    };
    if active_state_changed {
        sync_autostart_enabled(app, has_active);
    }
    // Pre-dispatch failures are just as user-visible as execution failures.
    // Emit only after releasing the persistence locks: notification delivery
    // can await network IO (for example WeChat push) and must not stall the
    // scheduler's durable state.
    for rejected in rejected_dispatches {
        emit_run_update(
            app,
            &rejected.automation,
            &rejected.record_id,
            &rejected.cwd,
            RunUpdate::Failed {
                session_id: None,
                error: &rejected.error,
            },
        );
        notify_run_failure(app, &rejected.automation, &rejected.error).await;
    }
    for dispatch in dispatches {
        emit_run_update(
            app,
            &dispatch.automation,
            &dispatch.record_id,
            &dispatch.cwd,
            RunUpdate::Queued,
        );
        tauri::async_runtime::spawn(execute_automation_run(
            app.clone(),
            tx.clone(),
            dispatch.automation,
            dispatch.cwd,
            dispatch.record_id,
        ));
    }
}

/// Start a scheduler bound to one runtime. The caller owns and replaces the
/// returned handle when the runtime restarts.
pub fn start_scheduler(
    app: AppHandle,
    tx: echo_agent_acp::AcpAgentTx,
    default_cwd: PathBuf,
) -> tauri::async_runtime::JoinHandle<()> {
    retry_terminal_completions(&app);
    fail_stale_running_records();
    tauri::async_runtime::spawn(async move {
        // Give the renderer time to subscribe to interaction events before a
        // recovered unattended turn can request input.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
        match recover_queued_dispatches(&default_cwd, &filesystem) {
            Ok(dispatches) => {
                for dispatch in dispatches {
                    tauri::async_runtime::spawn(execute_automation_run(
                        app.clone(),
                        tx.clone(),
                        dispatch.automation,
                        dispatch.cwd,
                        dispatch.record_id,
                    ));
                }
            }
            Err(error) => {
                tracing::error!(%error, "failed to recover queued automation runs");
            }
        }
        let mut interval = tokio::time::interval(AUTOMATION_SCHEDULER_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            retry_terminal_completions(&app);
            scheduler_tick(&app, &tx, &default_cwd).await;
        }
    })
}

// ---------- unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn edits_cannot_recreate_deleted_tasks_or_rewind_scheduler_metadata() {
        let mut store = AutomationStore::default();
        let mut created = test_automation();
        created.id = "owned-task".into();
        created.created_at = "2026-10-17T08:00:00+08:00".into();
        save_automation_in_store(&mut store, created.clone(), true).unwrap();
        let mut draft = created;
        store.automations[0].last_run_at = Some("2026-10-17T09:00:00+08:00".into());
        draft.name = "edited name".into();
        draft.created_at.clear();
        let saved = save_automation_in_store(&mut store, draft.clone(), false).unwrap();
        assert_eq!(saved.name, "edited name");
        assert_eq!(saved.created_at, "2026-10-17T08:00:00+08:00");
        assert_eq!(
            saved.last_run_at.as_deref(),
            Some("2026-10-17T09:00:00+08:00")
        );
        // A delete after the frontend snapshot was read is still authoritative.
        store.automations.clear();
        assert!(save_automation_in_store(&mut store, draft, false)
            .unwrap_err()
            .contains("已被移除"));
        assert!(store.automations.is_empty());
    }

    #[test]
    fn explicit_validation_modes_never_write_the_system_login_item() {
        use std::cell::Cell;
        let writes = Cell::new(0);
        for (review, packaged) in [(Some("1"), None), (None, Some("1")), (Some("1"), Some("1"))] {
            for enabled in [false, true] {
                let result = with_autostart_write_permission(review, packaged, || {
                    writes.set(writes.get() + 1);
                    enabled
                });
                assert!(result.is_none());
            }
        }
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn ordinary_profiles_keep_the_existing_autostart_behavior() {
        for (review, packaged) in [(None, None), (Some("0"), None), (None, Some("0"))] {
            for enabled in [false, true] {
                assert_eq!(
                    with_autostart_write_permission(review, packaged, || enabled),
                    Some(enabled)
                );
            }
        }
    }

    /// Helper: build a local DateTime from components.
    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid local datetime")
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn daily_schedule_uses_calendar_days_across_dst() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "automations::tests::dst_calendar_child",
                "--nocapture",
            ])
            .env("TZ", "America/New_York")
            .env("ECHO_DST_REVIEW_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
    }

    #[cfg(unix)]
    #[test]
    fn dst_calendar_child() {
        if std::env::var("ECHO_DST_REVIEW_CHILD").as_deref() != Ok("1") {
            return;
        }
        let sched = AutomationSchedule {
            freq: ScheduleFreq::DAILY,
            byhour: 9,
            ..Default::default()
        };
        // Adding 24 hours to 23:30 on the eve of a 23-hour day reaches
        // Monday. Calendar-day scheduling must still find Sunday at 09:00.
        let from = local(2026, 3, 7, 23, 30);
        assert_eq!(
            recurring_next(&sched, &from, from),
            Some(local(2026, 3, 8, 9, 0))
        );
        let from = local(2026, 10, 31, 23, 30);
        assert_eq!(
            recurring_next(&sched, &from, from),
            Some(local(2026, 11, 1, 9, 0))
        );
    }

    // --- parse_hhmm ---

    #[test]
    fn parse_hhmm_valid() {
        assert_eq!(parse_hhmm("09:30"), Some((9, 30)));
        assert_eq!(parse_hhmm("00:00"), Some((0, 0)));
        assert_eq!(parse_hhmm("23:59"), Some((23, 59)));
    }

    #[test]
    fn parse_hhmm_invalid() {
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("12:30:00"), None);
        assert_eq!(parse_hhmm("abc"), None);
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(parse_hhmm("12"), None);
    }

    // --- parse_date ---

    #[test]
    fn parse_date_valid() {
        assert_eq!(parse_date("2026-07-01"), Some(date(2026, 7, 1)));
        assert_eq!(parse_date(" 2026-01-15 "), Some(date(2026, 1, 15)));
    }

    #[test]
    fn parse_date_invalid() {
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date(""), None);
    }

    // --- weekday_code ---

    #[test]
    fn weekday_code_all_days() {
        // 2026-07-06 is Monday
        assert_eq!(weekday_code(date(2026, 7, 6)), "MO");
        assert_eq!(weekday_code(date(2026, 7, 7)), "TU");
        assert_eq!(weekday_code(date(2026, 7, 8)), "WE");
        assert_eq!(weekday_code(date(2026, 7, 9)), "TH");
        assert_eq!(weekday_code(date(2026, 7, 10)), "FR");
        assert_eq!(weekday_code(date(2026, 7, 11)), "SA");
        assert_eq!(weekday_code(date(2026, 7, 12)), "SU");
    }

    // --- week_start ---

    #[test]
    fn week_start_returns_monday() {
        // Wednesday 2026-07-08 → Monday 2026-07-06
        assert_eq!(week_start(date(2026, 7, 8)), date(2026, 7, 6));
        // Monday itself
        assert_eq!(week_start(date(2026, 7, 6)), date(2026, 7, 6));
        // Sunday 2026-07-12 → Monday 2026-07-06
        assert_eq!(week_start(date(2026, 7, 12)), date(2026, 7, 6));
    }

    // --- recurring_next: DAILY ---

    #[test]
    fn daily_next_same_day_future() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::DAILY,
            byhour: 14,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 7, 1, 9, 0);
        let from = local(2026, 7, 6, 10, 0); // before 14:00
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 7, 6, 14, 0));
    }

    #[test]
    fn daily_next_rolls_to_tomorrow() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::DAILY,
            byhour: 9,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 7, 1, 9, 0);
        let from = local(2026, 7, 6, 10, 0); // after 09:00
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 7, 7, 9, 0));
    }

    // --- recurring_next: WEEKLY ---

    #[test]
    fn weekly_next_matching_day() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::WEEKLY,
            interval: 1,
            byday: vec!["WE".into()],
            byhour: 10,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 7, 6, 9, 0); // Monday
        let from = local(2026, 7, 6, 12, 0); // Monday noon
        let next = recurring_next(&sched, &anchor, from).unwrap();
        // Next Wednesday = 2026-07-08
        assert_eq!(next, local(2026, 7, 8, 10, 0));
    }

    #[test]
    fn weekly_biweek_skips_alternate_weeks() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::WEEKLY,
            interval: 2,
            byday: vec!["MO".into()],
            byhour: 9,
            byminute: 0,
            ..Default::default()
        };
        // Anchor week starts Mon 2026-07-06.
        let anchor = local(2026, 7, 6, 9, 0);
        // From Monday 2026-07-13 (next week, odd offset from anchor week).
        let from = local(2026, 7, 13, 10, 0);
        let next = recurring_next(&sched, &anchor, from).unwrap();
        // Should skip to Mon 2026-07-20 (even weeks from anchor).
        assert_eq!(next, local(2026, 7, 20, 9, 0));
    }

    // --- recurring_next: MONTHLY ---

    #[test]
    fn monthly_next_matching_day() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::MONTHLY,
            bymonthday: vec![15],
            byhour: 8,
            byminute: 30,
            ..Default::default()
        };
        let anchor = local(2026, 7, 1, 9, 0);
        let from = local(2026, 7, 10, 12, 0);
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 7, 15, 8, 30));
    }

    #[test]
    fn monthly_rolls_to_next_month() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::MONTHLY,
            bymonthday: vec![5],
            byhour: 9,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 7, 1, 9, 0);
        let from = local(2026, 7, 10, 12, 0); // past the 5th
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 8, 5, 9, 0));
    }

    #[test]
    fn monthly_empty_bymonthday_returns_none() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::MONTHLY,
            bymonthday: vec![],
            ..Default::default()
        };
        let anchor = local(2026, 7, 1, 9, 0);
        let from = local(2026, 7, 10, 12, 0);
        assert!(recurring_next(&sched, &anchor, from).is_none());
    }

    // --- recurring_next: YEARLY ---

    #[test]
    fn yearly_next_this_year() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::YEARLY,
            bymonth: vec![12],
            bymonthday: vec![25],
            byhour: 10,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 1, 1, 9, 0);
        let from = local(2026, 7, 1, 12, 0);
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 12, 25, 10, 0));
    }

    #[test]
    fn yearly_rolls_to_next_year() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::YEARLY,
            bymonth: vec![3],
            bymonthday: vec![1],
            byhour: 9,
            byminute: 0,
            ..Default::default()
        };
        let anchor = local(2026, 1, 1, 9, 0);
        let from = local(2026, 7, 1, 12, 0); // past March
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2027, 3, 1, 9, 0));
    }

    #[test]
    fn yearly_leap_day_skips_non_leap_years() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::YEARLY,
            bymonth: vec![2],
            bymonthday: vec![29],
            byhour: 8,
            byminute: 15,
            ..Default::default()
        };
        let anchor = local(2026, 1, 1, 9, 0);
        let from = local(2027, 3, 1, 12, 0);
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2028, 2, 29, 8, 15));
    }

    // --- recurring_next: HOURLY ---

    #[test]
    fn hourly_next_within_day() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::HOURLY,
            interval_hours: 3,
            byday: ALL_DAYS.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let anchor = local(2026, 7, 6, 0, 0);
        let from = local(2026, 7, 6, 7, 30); // between 06:00 and 09:00
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 7, 6, 9, 0));
    }

    #[test]
    fn hourly_respects_configured_start_and_minute() {
        let sched = AutomationSchedule {
            freq: ScheduleFreq::HOURLY,
            interval_hours: 3,
            byday: ALL_DAYS.iter().map(|s| s.to_string()).collect(),
            byhour: 9,
            byminute: 30,
            ..Default::default()
        };
        let anchor = local(2026, 7, 6, 0, 0);
        let from = local(2026, 7, 6, 10, 0);
        let next = recurring_next(&sched, &anchor, from).unwrap();
        assert_eq!(next, local(2026, 7, 6, 12, 30));
    }

    // --- compute_next_run ---

    #[test]
    fn compute_next_run_paused_returns_none() {
        let a = Automation {
            id: "1".into(),
            name: "test".into(),
            prompt: "p".into(),
            status: "PAUSED".into(),
            created_at: "2026-01-01T00:00:00+08:00".into(),
            ..test_automation()
        };
        assert!(compute_next_run(&a, local(2026, 7, 6, 10, 0)).is_none());
    }

    #[test]
    fn compute_next_run_once_future() {
        let a = Automation {
            id: "1".into(),
            name: "once".into(),
            prompt: "p".into(),
            schedule_type: "once".into(),
            scheduled_date: Some("2026-12-25".into()),
            scheduled_time: Some("10:00".into()),
            created_at: "2026-01-01T00:00:00+08:00".into(),
            ..test_automation()
        };
        let result = compute_next_run(&a, local(2026, 7, 6, 10, 0));
        assert!(result.is_some());
    }

    #[test]
    fn compute_next_run_once_past_returns_none() {
        let a = Automation {
            id: "1".into(),
            name: "once".into(),
            prompt: "p".into(),
            schedule_type: "once".into(),
            scheduled_date: Some("2020-01-01".into()),
            scheduled_time: Some("10:00".into()),
            created_at: "2020-01-01T00:00:00+08:00".into(),
            ..test_automation()
        };
        assert!(compute_next_run(&a, local(2026, 7, 6, 10, 0)).is_none());
    }

    #[test]
    fn compute_next_run_expired_validity_returns_none() {
        let a = Automation {
            id: "1".into(),
            name: "expired".into(),
            prompt: "p".into(),
            valid_until_date: Some("2026-06-30".into()),
            created_at: "2026-01-01T00:00:00+08:00".into(),
            ..test_automation()
        };
        // from is after valid_until
        assert!(compute_next_run(&a, local(2026, 7, 6, 10, 0)).is_none());
    }

    #[test]
    fn claim_due_preserves_future_and_advances_due_recurring() {
        let now = local(2026, 7, 6, 10, 0);
        let mut due = Automation {
            id: "due".into(),
            name: "due".into(),
            prompt: "run".into(),
            next_run_at: Some(local(2026, 7, 6, 9, 0).to_rfc3339()),
            schedule: AutomationSchedule {
                freq: ScheduleFreq::DAILY,
                byhour: 9,
                byminute: 0,
                ..Default::default()
            },
            created_at: local(2026, 1, 1, 9, 0).to_rfc3339(),
            ..test_automation()
        };
        let future = Automation {
            id: "future".into(),
            name: "future".into(),
            prompt: "later".into(),
            next_run_at: Some(local(2026, 7, 6, 11, 0).to_rfc3339()),
            created_at: local(2026, 1, 1, 9, 0).to_rfc3339(),
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![due.clone(), future],
        };
        let claimed = claim_due(&mut store, now);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "due");
        due.next_run_at = Some(local(2026, 7, 7, 9, 0).to_rfc3339());
        assert_eq!(store.automations[0].next_run_at, due.next_run_at);
        assert_eq!(
            store.automations[1].next_run_at,
            Some(local(2026, 7, 6, 11, 0).to_rfc3339())
        );
    }

    #[test]
    fn claim_due_pauses_once_task_and_cannot_claim_twice() {
        let now = local(2026, 7, 6, 10, 0);
        let once = Automation {
            id: "once".into(),
            name: "once".into(),
            prompt: "run".into(),
            schedule_type: "once".into(),
            scheduled_date: Some("2026-07-06".into()),
            scheduled_time: Some("09:00".into()),
            next_run_at: Some(local(2026, 7, 6, 9, 0).to_rfc3339()),
            created_at: local(2026, 1, 1, 9, 0).to_rfc3339(),
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![once],
        };
        assert_eq!(claim_due(&mut store, now).len(), 1);
        assert_eq!(store.automations[0].status, "PAUSED");
        assert!(store.automations[0].next_run_at.is_none());
        assert!(claim_due(&mut store, now + Duration::minutes(1)).is_empty());
    }

    #[test]
    fn claim_due_does_not_advance_an_automation_with_a_scheduled_run_in_flight() {
        let now = local(2026, 7, 6, 10, 0);
        let scheduled_for = local(2026, 7, 6, 9, 0).to_rfc3339();
        let automation = Automation {
            id: "busy".into(),
            name: "busy".into(),
            prompt: "run".into(),
            next_run_at: Some(scheduled_for.clone()),
            created_at: local(2026, 1, 1, 9, 0).to_rfc3339(),
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![automation],
        };
        let blocked = HashSet::from(["busy".to_string()]);

        assert!(claim_due_unblocked(&mut store, now, &blocked).is_empty());
        assert_eq!(
            store.automations[0].next_run_at.as_deref(),
            Some(scheduled_for.as_str())
        );
        assert!(store.automations[0].last_run_at.is_none());

        let claimed = claim_due_unblocked(&mut store, now, &HashSet::new());
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].scheduled_for, scheduled_for);
    }

    #[test]
    fn changing_one_status_does_not_skip_an_unrelated_due_occurrence() {
        let now = local(2026, 7, 6, 10, 0);
        let due_at = local(2026, 7, 6, 9, 0).to_rfc3339();
        let paused = Automation {
            id: "paused".into(),
            name: "paused".into(),
            prompt: "run".into(),
            status: "PAUSED".into(),
            ..test_automation()
        };
        let due = Automation {
            id: "due".into(),
            name: "due".into(),
            prompt: "run".into(),
            next_run_at: Some(due_at.clone()),
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![paused, due],
        };

        set_status_at(&mut store, "paused", "ACTIVE", now).unwrap();

        assert_eq!(
            store.automations[1].next_run_at.as_deref(),
            Some(due_at.as_str())
        );
        assert_eq!(claim_due(&mut store, now).len(), 1);
        assert_eq!(
            store.automations[1].last_run_at.as_deref(),
            Some(now.to_rfc3339().as_str())
        );
    }

    #[test]
    fn recovered_occurrence_advances_only_the_matching_schedule() {
        let now = local(2026, 7, 6, 10, 0);
        let scheduled_for = local(2026, 7, 6, 9, 0).to_rfc3339();
        let mut automation = Automation {
            id: "recover".into(),
            name: "recover".into(),
            prompt: "run".into(),
            next_run_at: Some(scheduled_for.clone()),
            schedule: AutomationSchedule {
                freq: ScheduleFreq::DAILY,
                byhour: 9,
                byminute: 0,
                ..Default::default()
            },
            created_at: local(2026, 1, 1, 9, 0).to_rfc3339(),
            ..test_automation()
        };

        assert!(!advance_recovered_occurrence(
            &mut automation,
            "2026-07-05T09:00:00+08:00",
            "started",
            &now,
        ));
        assert_eq!(
            automation.next_run_at.as_deref(),
            Some(scheduled_for.as_str())
        );

        assert!(advance_recovered_occurrence(
            &mut automation,
            &scheduled_for,
            "started",
            &now,
        ));
        assert_eq!(automation.last_run_at.as_deref(), Some("started"));
        assert_eq!(
            automation.next_run_at,
            Some(local(2026, 7, 7, 9, 0).to_rfc3339())
        );
    }

    #[test]
    fn queued_record_contains_recovery_context_and_legacy_records_still_load() {
        let mut records = RunRecordStore::default();
        let automation = Automation {
            id: "automation".into(),
            name: "daily report".into(),
            model_id: None,
            ..test_automation()
        };
        let scheduled_for = "2026-07-06T09:00:00+08:00";
        let id = append_run_started(
            &mut records,
            &automation,
            "2026-07-06T10:00:00+08:00",
            Path::new("/workspace"),
            Some(scheduled_for),
            Some("resolved-model"),
        );
        let record = records.records.first().unwrap();
        assert_eq!(record.id, id);
        assert_eq!(record.status, "queued");
        assert_eq!(record.cwd.as_deref(), Some("/workspace"));
        assert_eq!(record.model_id.as_deref(), Some("resolved-model"));
        assert_eq!(
            record
                .automation_snapshot
                .as_ref()
                .map(|item| item.id.as_str()),
            Some("automation")
        );
        assert!(record
            .automation_snapshot
            .as_ref()
            .and_then(|item| item.model_id.as_deref())
            .is_none());
        assert_eq!(record.scheduled_for.as_deref(), Some(scheduled_for));

        let legacy: AutomationRunRecord = serde_json::from_value(serde_json::json!({
            "id": "old",
            "automationId": "automation",
            "automationName": "old run",
            "status": "success",
            "startedAt": "2025-01-01T00:00:00+08:00"
        }))
        .unwrap();
        assert!(legacy.cwd.is_none());
        assert!(legacy.model_id.is_none());
        assert!(legacy.automation_snapshot.is_none());
        assert!(legacy.scheduled_for.is_none());
    }

    #[test]
    fn manual_run_does_not_hide_the_first_due_scheduled_occurrence() {
        let mut records = RunRecordStore::default();
        let manual = Automation {
            id: "manual-active".into(),
            ..test_automation()
        };
        let scheduled = Automation {
            id: "scheduled-active".into(),
            ..test_automation()
        };
        append_run_started(
            &mut records,
            &manual,
            "2026-07-06T09:55:00+08:00",
            Path::new("/workspace"),
            None,
            Some("model"),
        );
        append_run_started(
            &mut records,
            &scheduled,
            "2026-07-06T10:00:00+08:00",
            Path::new("/workspace"),
            Some("2026-07-06T10:00:00+08:00"),
            Some("model"),
        );

        let blocked = scheduled_runs_in_flight(&records);
        assert!(!blocked.contains("manual-active"));
        assert!(blocked.contains("scheduled-active"));
    }

    #[tokio::test]
    async fn occurrences_of_the_same_automation_are_serialized() {
        let first = automation_run_lock("serialization-test");
        let second = automation_run_lock("serialization-test");
        assert!(Arc::ptr_eq(&first, &second));

        let guard = first.lock().await;
        assert!(second.try_lock().is_err());
        drop(guard);
        assert!(second.try_lock().is_ok());
    }

    #[test]
    fn response_and_notification_order_agree_and_claim_only_once() {
        for response_first in [true, false] {
            let response = agent_client_protocol::PromptResponse::new(
                agent_client_protocol::StopReason::Cancelled,
            )
            .meta(Some(serde_json::Map::from_iter([(
                "cancellationCategory".into(),
                serde_json::json!("HookDenied"),
            )])));
            let response_event = crate::agent_runtime::completion_from_prompt_response(
                response, "session", "prompt",
            )
            .unwrap();
            let notification = crate::bridge::parse_complete_event(
                &serde_json::json!({"sessionId":"session","promptId":"prompt","stopReason":"cancelled","_meta":{"cancellationCategory":"HookDenied"}}),
            );
            let events = if response_first {
                [response_event, notification]
            } else {
                [notification, response_event]
            };
            let mut records = RunRecordStore::default();
            append_run_started(
                &mut records,
                &test_automation(),
                "start",
                Path::new("/workspace"),
                None,
                Some("model"),
            );
            let record = records.records.first_mut().unwrap();
            let mut notifications = 0;
            for event in events {
                let ok = crate::bridge::completion_disposition(&event)
                    == crate::bridge::CompletionDisposition::Success;
                notifications += usize::from(finalize_record(
                    record,
                    ok,
                    "end",
                    Some("session"),
                    event.cancellation_category.as_deref(),
                ));
            }
            assert_eq!(record.status, "failed");
            assert_eq!(record.error.as_deref(), Some("HookDenied"));
            assert_eq!(notifications, 1);
        }
    }

    #[test]
    fn late_dispatch_completion_cannot_invert_a_terminal_failure() {
        let mut records = RunRecordStore::default();
        let automation = Automation {
            id: "automation".into(),
            name: "daily report".into(),
            ..test_automation()
        };
        append_run_started(
            &mut records,
            &automation,
            "2026-07-06T10:00:00+08:00",
            Path::new("/workspace"),
            None,
            Some("model"),
        );
        let record = records.records.first_mut().unwrap();
        assert!(finalize_record(
            record,
            false,
            "2026-07-06T10:01:00+08:00",
            Some("session"),
            Some("rate_limit"),
        ));
        assert!(!finalize_record(
            record,
            true,
            "2026-07-06T10:02:00+08:00",
            Some("session"),
            None,
        ));
        assert_eq!(record.status, "failed");
        assert_eq!(record.error.as_deref(), Some("rate_limit"));
        assert_eq!(
            record.finished_at.as_deref(),
            Some("2026-07-06T10:01:00+08:00")
        );
    }

    #[test]
    fn ensure_next_runs_pauses_finished_active_schedules() {
        let now = local(2026, 7, 6, 10, 0);
        let expired_recurring = Automation {
            id: "expired".into(),
            name: "expired".into(),
            prompt: "run".into(),
            valid_until_date: Some("2026-06-30".into()),
            next_run_at: None,
            ..test_automation()
        };
        let missed_once = Automation {
            id: "once".into(),
            name: "once".into(),
            prompt: "run".into(),
            schedule_type: "once".into(),
            scheduled_date: Some("2026-07-05".into()),
            scheduled_time: Some("09:00".into()),
            next_run_at: None,
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![expired_recurring, missed_once],
        };

        ensure_next_runs(&mut store, now);

        assert!(store
            .automations
            .iter()
            .all(|automation| automation.status == "PAUSED"));
    }

    #[test]
    fn ensure_next_runs_repairs_a_malformed_persisted_timestamp() {
        let now = local(2026, 7, 6, 10, 0);
        let automation = Automation {
            id: "damaged".into(),
            name: "damaged".into(),
            prompt: "run".into(),
            next_run_at: Some("not-a-timestamp".into()),
            schedule: AutomationSchedule {
                freq: ScheduleFreq::DAILY,
                byhour: 11,
                byminute: 0,
                ..Default::default()
            },
            ..test_automation()
        };
        let mut store = AutomationStore {
            automations: vec![automation],
        };

        ensure_next_runs(&mut store, now);

        assert_eq!(
            store.automations[0].next_run_at,
            Some(local(2026, 7, 6, 11, 0).to_rfc3339())
        );
    }

    // --- first_cwd ---

    #[test]
    fn first_cwd_extracts_first_entry() {
        let a = Automation {
            cwds: "/home/user, /tmp".into(),
            ..test_automation()
        };
        assert_eq!(first_cwd(&a), Some("/home/user".into()));
    }

    #[test]
    fn first_cwd_empty_returns_none() {
        let a = Automation {
            cwds: "".into(),
            ..test_automation()
        };
        assert_eq!(first_cwd(&a), None);
    }

    #[test]
    fn bounded_text_reader_rejects_files_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_bounded_string(&path, 4)
            .unwrap_err()
            .contains("safety limit"));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_text_reader_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        let link = dir.path().join("linked.json");
        std::fs::write(&target, b"{}").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_bounded_string(&link, 128)
            .unwrap_err()
            .contains("non-symlink"));
    }

    #[test]
    fn automation_payload_cannot_self_authorize_a_forged_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let trusted = tmp.path().join("trusted");
        let forged = tmp.path().join("forged");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&forged).unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        filesystem
            .authorize_workspace(&trusted.to_string_lossy())
            .unwrap();

        assert!(
            normalize_authorized_cwds(&filesystem, &forged.to_string_lossy())
                .unwrap_err()
                .contains("未经用户授权")
        );
        assert_eq!(
            normalize_authorized_cwds(&filesystem, &trusted.to_string_lossy()).unwrap(),
            trusted.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn persisted_full_access_automation_still_requires_a_live_native_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let trusted = tmp.path().join("trusted");
        let forged = tmp.path().join("forged");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&forged).unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        filesystem
            .authorize_workspace(&trusted.to_string_lossy())
            .unwrap();
        let forged_task = Automation {
            cwds: forged.to_string_lossy().into_owned(),
            permission_mode: "fullAccess".into(),
            ..test_automation()
        };
        assert!(require_automation_cwd(&filesystem, &forged_task, &trusted).is_err());

        let trusted_task = Automation {
            cwds: trusted.to_string_lossy().into_owned(),
            permission_mode: "fullAccess".into(),
            ..test_automation()
        };
        assert_eq!(
            require_automation_cwd(&filesystem, &trusted_task, &forged).unwrap(),
            trusted.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn failed_created_automation_binding_revokes_permissions_before_cancel() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            commit_created_automation_session(
                &client.tx,
                "new-unrecorded-full-access-session",
                true,
                "ask",
                || Err("injected workspace metadata write failure".into()),
                || Ok(()),
                |_| panic!("acknowledged permission reset must not quarantine"),
            )
            .await
        });
        let echo_agent_acp::AcpAgentMessage::ExtNotification(reset) = agent
            .rx
            .recv()
            .await
            .expect("workspace binding failure must revoke launch permissions")
        else {
            panic!("failed workspace binding must first revoke launch authority");
        };
        assert_eq!(
            reset.request.method.as_ref(),
            "echo.agent/yolo_mode_changed"
        );
        let params: serde_json::Value = serde_json::from_str(reset.request.params.get()).unwrap();
        assert_eq!(
            params["sessionIds"],
            serde_json::json!(["new-unrecorded-full-access-session"])
        );
        assert_eq!(params["permission_mode"], "ask");
        assert_eq!(params["yolo_mode"], false);
        assert_eq!(params["auto_mode"], false);
        assert!(
            !task.is_finished(),
            "cleanup must wait for Runtime reset ACK"
        );
        reset.response_tx.send(Ok(())).unwrap();
        let echo_agent_acp::AcpAgentMessage::Cancel(cancel) = agent
            .rx
            .recv()
            .await
            .expect("unrecorded Runtime session must be cancelled")
        else {
            panic!("unrecorded Runtime session must be cancelled after reset");
        };
        assert_eq!(
            cancel.request.session_id.0.as_ref(),
            "new-unrecorded-full-access-session"
        );
        assert!(
            !task.is_finished(),
            "cleanup must wait for actor cancellation ACK"
        );
        cancel.response_tx.send(Ok(())).unwrap();
        let failure = task.await.unwrap().unwrap_err();
        assert_eq!(failure.error, "injected workspace metadata write failure");
        assert!(failure.permission_reset_error.is_none());
    }

    #[tokio::test]
    async fn created_automation_commit_preserves_success_and_rolls_back_run_record_failure() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let committed = std::cell::RefCell::new(Vec::new());
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            commit_created_automation_session(
                &client.tx,
                "healthy-session",
                true,
                "ask",
                || {
                    committed.borrow_mut().push("workspace");
                    Ok(())
                },
                || {
                    committed.borrow_mut().push("run");
                    Ok(())
                },
                |_| panic!("successful commit must not quarantine"),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(*committed.borrow(), vec!["workspace", "run"]);
        assert!(
            agent.rx.try_recv().is_err(),
            "successful commits must not revoke or cancel the session"
        );

        let task = tokio::spawn(async move {
            commit_created_automation_session(
                &client.tx,
                "record-failure",
                false,
                "ask",
                || Ok(()),
                || Err("run record write failed".into()),
                |_| panic!("default-mode failure must not quarantine"),
            )
            .await
        });
        let echo_agent_acp::AcpAgentMessage::Cancel(cancel) = agent.rx.recv().await.unwrap() else {
            panic!("run record failure must retain its existing cancellation behavior");
        };
        assert_eq!(cancel.request.session_id.0.as_ref(), "record-failure");
        cancel.response_tx.send(Ok(())).unwrap();
        assert_eq!(
            task.await.unwrap().unwrap_err().error,
            "run record write failed"
        );
    }

    #[tokio::test]
    async fn failed_created_session_reports_cancellation_failure_without_hiding_commit_error() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            commit_created_automation_session(
                &client.tx,
                "cancel-failure",
                false,
                "ask",
                || Err("workspace write failed".into()),
                || Ok(()),
                |_| panic!("default-mode failure must not quarantine"),
            )
            .await
        });
        let echo_agent_acp::AcpAgentMessage::Cancel(cancel) = agent.rx.recv().await.unwrap() else {
            panic!("failed binding must attempt cancellation");
        };
        cancel
            .response_tx
            .send(Err(
                agent_client_protocol::Error::internal_error().data("cancel rejected")
            ))
            .unwrap();
        let failure = task.await.unwrap().unwrap_err();
        assert!(failure.error.contains("workspace write failed"));
        assert!(failure.error.contains("cancel rejected"));
    }

    #[tokio::test]
    async fn failed_created_default_automation_binding_still_cancels_session() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            commit_created_automation_session(
                &client.tx,
                "unrecorded-default-session",
                false,
                "ask",
                || Err("injected workspace binding failure".into()),
                || Ok(()),
                |_| panic!("default-mode failure must not quarantine"),
            )
            .await
        });
        let echo_agent_acp::AcpAgentMessage::Cancel(cancel) = agent.rx.recv().await.unwrap() else {
            panic!("ordinary unrecorded session must be cancelled without permission mutation");
        };
        assert_eq!(
            cancel.request.session_id.0.as_ref(),
            "unrecorded-default-session"
        );
        cancel.response_tx.send(Ok(())).unwrap();
        let failure = task.await.unwrap().unwrap_err();
        assert_eq!(failure.error, "injected workspace binding failure");
        assert!(failure.permission_reset_error.is_none());
    }

    #[tokio::test]
    async fn failed_created_session_reset_still_cancels_and_requires_quarantine() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let quarantined = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let quarantine_in_task = quarantined.clone();
        let task = tokio::spawn(async move {
            commit_created_automation_session(
                &client.tx,
                "unrecorded-session",
                true,
                "ask",
                || Err("injected metadata write failure".into()),
                || Ok(()),
                move |error| {
                    assert!(error.contains("injected permission reset failure"));
                    quarantine_in_task.store(true, std::sync::atomic::Ordering::SeqCst);
                },
            )
            .await
        });
        for _ in 0..2 {
            let echo_agent_acp::AcpAgentMessage::ExtNotification(reset) =
                agent.rx.recv().await.unwrap()
            else {
                panic!("permission reset must be retried");
            };
            reset
                .response_tx
                .send(Err(agent_client_protocol::Error::internal_error()
                    .data("injected permission reset failure")))
                .unwrap();
        }
        let echo_agent_acp::AcpAgentMessage::Cancel(cancel) = agent.rx.recv().await.unwrap() else {
            panic!("reset failure must not skip cancellation");
        };
        assert!(
            quarantined.load(std::sync::atomic::Ordering::SeqCst),
            "quarantine must happen before waiting for cancel ACK"
        );
        cancel.response_tx.send(Ok(())).unwrap();
        let failure = task.await.unwrap().unwrap_err();
        assert!(failure
            .permission_reset_error
            .unwrap()
            .contains("injected permission reset failure"));
        assert_eq!(failure.error, "injected metadata write failure");
    }

    // --- blank_to_none ---

    #[test]
    fn blank_to_none_normalizes() {
        let mut v = Some("  ".to_string());
        blank_to_none(&mut v);
        assert_eq!(v, None);

        let mut v2 = Some("hello".to_string());
        blank_to_none(&mut v2);
        assert_eq!(v2, Some("hello".into()));

        let mut v3: Option<String> = None;
        blank_to_none(&mut v3);
        assert_eq!(v3, None);
    }

    #[test]
    fn validation_rejects_expired_active_window() {
        let automation = Automation {
            name: "expired".into(),
            prompt: "run".into(),
            valid_until_date: Some("2026-06-30".into()),
            ..test_automation()
        };
        assert_eq!(
            validate_automation_at(&automation, local(2026, 7, 6, 10, 0)),
            Err("生效日期区间已过期，请更新结束日期".into())
        );
    }

    #[test]
    fn validation_allows_editing_finished_paused_once_task() {
        let automation = Automation {
            name: "finished".into(),
            prompt: "run".into(),
            status: "PAUSED".into(),
            schedule_type: "once".into(),
            scheduled_date: Some("2026-07-05".into()),
            scheduled_time: Some("09:00".into()),
            ..test_automation()
        };
        assert!(validate_automation_at(&automation, local(2026, 7, 6, 10, 0)).is_ok());
    }

    #[test]
    fn validation_accepts_valid_leap_day_schedule() {
        let automation = Automation {
            name: "leap".into(),
            prompt: "run".into(),
            schedule: AutomationSchedule {
                freq: ScheduleFreq::YEARLY,
                bymonth: vec![2],
                bymonthday: vec![29],
                ..Default::default()
            },
            ..test_automation()
        };
        assert!(validate_automation_at(&automation, local(2026, 7, 6, 10, 0)).is_ok());
    }

    // --- migrate_legacy_json ---

    #[test]
    fn migrate_legacy_daily() {
        let mut json = serde_json::json!({
            "automations": [{
                "id": "1",
                "name": "old",
                "prompt": "p",
                "status": "active",
                "cwd": "/home",
                "createdAt": "2025-01-01T00:00:00+08:00",
                "schedule": { "type": "daily", "time": "08:30" }
            }]
        });
        migrate_legacy_json(&mut json);
        let item = &json["automations"][0];
        assert_eq!(item["status"], "ACTIVE");
        assert_eq!(item["cwds"], "/home");
        assert_eq!(item["schedule"]["freq"], "DAILY");
        assert_eq!(item["schedule"]["byhour"], 8);
        assert_eq!(item["schedule"]["byminute"], 30);
    }

    #[test]
    fn migrate_legacy_weekly_with_weekdays() {
        let mut json = serde_json::json!({
            "automations": [{
                "id": "2",
                "name": "weekly",
                "prompt": "p",
                "status": "paused",
                "createdAt": "2025-01-01T00:00:00+08:00",
                "schedule": { "type": "weekly", "time": "17:00", "weekdays": [1, 5] }
            }]
        });
        migrate_legacy_json(&mut json);
        let sched = &json["automations"][0]["schedule"];
        assert_eq!(sched["freq"], "WEEKLY");
        // 1=MO, 5=FR in the legacy 0=Sunday mapping
        let byday = sched["byday"].as_array().unwrap();
        assert!(byday.contains(&serde_json::json!("MO")));
        assert!(byday.contains(&serde_json::json!("FR")));
    }

    #[test]
    fn migrate_legacy_already_current_is_noop() {
        let mut json = serde_json::json!({
            "automations": [{
                "id": "3",
                "name": "current",
                "prompt": "p",
                "status": "ACTIVE",
                "createdAt": "2025-01-01T00:00:00+08:00",
                "schedule": { "freq": "DAILY", "interval": 1, "byday": ["MO"], "byhour": 9, "byminute": 0 }
            }]
        });
        let before = json.clone();
        migrate_legacy_json(&mut json);
        // schedule.freq already present → no change to schedule
        assert_eq!(
            json["automations"][0]["schedule"],
            before["automations"][0]["schedule"]
        );
    }

    #[test]
    fn session_created_event_carries_resolved_model_for_auto_and_explicit_selection() {
        for configured in [None, Some("explicit-model".to_string())] {
            let mut automation = test_automation();
            automation.model_id = configured;
            let event = run_update_event(
                &automation,
                "record",
                Path::new("/workspace"),
                RunUpdate::SessionCreated("session", "actual-model"),
            );
            let json = serde_json::to_value(event).unwrap();
            assert_eq!(json["currentModelId"], "actual-model");
            assert_eq!(json["sessionId"], "session");
            assert_eq!(json["phase"], "sessionCreated");
        }
        // Completion must not overwrite a model changed later by the user.
        let finished = run_update_event(
            &test_automation(),
            "record",
            Path::new("/workspace"),
            RunUpdate::Failed {
                session_id: Some("session"),
                error: "stopped",
            },
        );
        assert!(serde_json::to_value(finished)
            .unwrap()
            .get("currentModelId")
            .is_none());
    }

    #[test]
    fn pre_dispatch_failure_event_is_terminal_and_keeps_the_reason() {
        let automation = test_automation();
        let event = run_update_event(
            &automation,
            "record-failed",
            Path::new("/workspace"),
            RunUpdate::Failed {
                session_id: None,
                error: "Auto 模型当前不可用",
            },
        );
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["phase"], "finished");
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"], "Auto 模型当前不可用");
        assert!(json.get("sessionId").is_none());
    }

    #[test]
    fn unattended_skills_require_a_current_runnable_preflight() {
        use echo_agent_tools::implementations::skills::capability::{
            SkillCapabilityReport, SkillCapabilityState,
        };

        let report = |state| SkillCapabilityReport {
            declared: state != SkillCapabilityState::InstructionOnly,
            state,
            ready: state.can_run_automated(),
            capabilities: Vec::new(),
            checks: Vec::new(),
            manifest: None,
        };
        assert!(require_automated_skill_capability(None, "demo")
            .unwrap_err()
            .contains("未完成执行能力预检"));
        for state in [
            SkillCapabilityState::InstructionOnly,
            SkillCapabilityState::Ready,
        ] {
            assert!(require_automated_skill_capability(Some(&report(state)), "demo").is_ok());
        }
        for state in [
            SkillCapabilityState::MissingDependencies,
            SkillCapabilityState::ConfigurationRequired,
            SkillCapabilityState::Invalid,
        ] {
            assert!(require_automated_skill_capability(Some(&report(state)), "demo").is_err());
        }
    }

    #[test]
    fn retired_builtin_automation_models_use_reed_without_changing_other_choices() {
        let selections = [
            None,
            Some("echoagent-ojlab/chat-xc"),
            Some("echoagent-ojlab/chat-glm"),
            Some("echoagent-ojlab/chat-qwen"),
            Some("mine/chat-qwen"),
            Some("echoagent-ojlab/Echo Grove"),
        ];
        let mut store = AutomationStore {
            automations: selections
                .iter()
                .map(|model| Automation {
                    model_id: model.map(str::to_string),
                    ..test_automation()
                })
                .collect(),
        };
        migrate_builtin_models(&mut store);
        assert_eq!(
            store
                .automations
                .iter()
                .map(|item| item.model_id.as_deref())
                .collect::<Vec<_>>(),
            vec![
                None,
                Some("echoagent-ojlab/Echo Reed"),
                Some("echoagent-ojlab/Echo Reed"),
                Some("echoagent-ojlab/Echo Reed"),
                Some("mine/chat-qwen"),
                Some("echoagent-ojlab/Echo Grove"),
            ],
        );
    }

    fn test_automation() -> Automation {
        Automation {
            id: String::new(),
            name: String::new(),
            prompt: String::new(),
            cwds: String::new(),
            status: "ACTIVE".into(),
            model_id: None,
            model_is_thinking: false,
            skills: vec![],
            expert_id: None,
            expert_name: None,
            expert_path: None,
            connector_ids: vec![],
            permission_mode: "fullAccess".into(),
            schedule_type: "recurring".into(),
            schedule: AutomationSchedule::default(),
            scheduled_date: None,
            scheduled_time: None,
            valid_from_date: None,
            valid_until_date: None,
            push_to_we_chat: false,
            last_run_at: None,
            next_run_at: None,
            created_at: String::new(),
        }
    }
    fn terminal_fixture() -> (RunRecordStore, TerminalFact) {
        let mut records = RunRecordStore::default();
        let id = append_run_started(
            &mut records,
            &test_automation(),
            "start",
            Path::new("/fixture"),
            Some("due"),
            Some("model"),
        );
        let record = records.records.first_mut().unwrap();
        record.status = "running".into();
        record.session_id = Some("session".into());
        record.prompt_id = Some("first-prompt".into());
        let fact = TerminalFact {
            record_id: id,
            session_id: Some("session".into()),
            prompt_id: Some("first-prompt".into()),
            ok: true,
            interrupted: false,
            error: None,
            finished_at: "end".into(),
            completion: None,
            local_notification_id: None,
            external_claimed: false,
            external_result: None,
            restored_orphan: false,
        };
        (records, fact)
    }

    #[test]
    fn restore_reconciles_a_durable_fact_after_history_failure_without_external_replay() {
        let home = tempfile::tempdir().unwrap();
        let (mut records, mut fact) = terminal_fixture();
        let history = serde_json::to_vec(&records).unwrap();
        let mut durable_fact = Vec::new();
        assert!(persist_terminal_transition(
            &mut records,
            &mut fact,
            |fact| {
                durable_fact = serde_json::to_vec(fact).unwrap();
                Ok(())
            },
            |_| Err("injected history failure".into())
        )
        .is_err());
        let outbox = home.path().join("automation-terminal-outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let path = outbox.join(format!(
            "{:x}.json",
            Sha256::digest(fact.record_id.as_bytes())
        ));
        std::fs::write(&path, durable_fact).unwrap();
        std::fs::write(
            home.path().join("echoagent-automation-records.json"),
            history,
        )
        .unwrap();
        prepare_restored_terminals(home.path()).unwrap();
        let records: RunRecordStore = serde_json::from_slice(
            &std::fs::read(home.path().join("echoagent-automation-records.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(records.records[0].status, "success");
        assert_eq!(records.records[0].finished_at.as_deref(), Some("end"));
        assert!(records.records[0].automation_snapshot.is_none());
        assert!(scheduled_runs_in_flight(&records).is_empty());
        let mut fact: TerminalFact =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(fact.completion.as_ref().unwrap().event.status, "success");
        assert!(!fact.completion.as_ref().unwrap().push);
        assert!(!claim_external_attempt(&mut fact, true, |_| Ok(())).unwrap());
        prepare_restored_terminals(home.path()).unwrap();
    }

    #[test]
    fn restore_accepts_unbound_failure_facts_and_suppresses_deleted_history() {
        let home = tempfile::tempdir().unwrap();
        let (records, mut fact) = terminal_fixture();
        fact.ok = false;
        fact.error = Some("execution timeout".into());
        fact.session_id = None;
        fact.prompt_id = None;
        let outbox = home.path().join("automation-terminal-outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let path = outbox.join(format!(
            "{:x}.json",
            Sha256::digest(fact.record_id.as_bytes())
        ));
        std::fs::write(&path, serde_json::to_vec(&fact).unwrap()).unwrap();
        let history_path = home.path().join("echoagent-automation-records.json");
        std::fs::write(&history_path, serde_json::to_vec(&records).unwrap()).unwrap();
        prepare_restored_terminals(home.path()).unwrap();
        let restored: RunRecordStore =
            serde_json::from_slice(&std::fs::read(&history_path).unwrap()).unwrap();
        assert_eq!(restored.records[0].status, "failed");
        assert_eq!(
            restored.records[0].error.as_deref(),
            Some("execution timeout")
        );
        let saved: TerminalFact = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!saved.completion.unwrap().push);
        std::fs::write(
            &history_path,
            serde_json::to_vec(&RunRecordStore::default()).unwrap(),
        )
        .unwrap();
        prepare_restored_terminals(home.path()).unwrap();
        let mut orphan: TerminalFact =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(orphan.restored_orphan);
        assert!(!claim_external_attempt(&mut orphan, true, |_| Ok(())).unwrap());
        assert!(
            serde_json::from_slice::<RunRecordStore>(&std::fs::read(&history_path).unwrap())
                .unwrap()
                .records
                .is_empty()
        );
    }

    #[test]
    fn manual_takeover_closes_original_occurrence_and_survives_publication_retry() {
        let (mut records, mut fact) = terminal_fixture();
        records.records[0].scheduled_for = Some("occurrence".into());
        fact.ok = false;
        fact.interrupted = true;
        fact.error = Some("已人工接管：新消息按普通会话继续执行".into());
        let mut saved = String::new();
        persist_terminal_transition(
            &mut records,
            &mut fact,
            |fact| {
                saved = serde_json::to_string(fact).unwrap();
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(records.records[0].status, "interrupted");
        assert!(scheduled_runs_in_flight(&records).is_empty());
        assert!(!matches_prompt(
            &records.records[0],
            "session",
            "replacement-prompt"
        ));
        let mut recovered: TerminalFact = serde_json::from_str(&saved).unwrap();
        persist_terminal_transition(
            &mut records,
            &mut recovered,
            |_| Ok(()),
            |_| panic!("do not reopen a closed occurrence"),
        )
        .unwrap();
        let completion = recovered.completion.unwrap();
        assert_eq!(completion.event.status, "interrupted");
        assert!(publication_title(&completion).ends_with("已人工接管"));
        assert!(!finalize_record(
            &mut records.records[0],
            true,
            "later",
            Some("session"),
            None
        ));
    }

    #[test]
    fn historical_automation_session_followup_is_not_the_original_prompt() {
        let (mut records, fact) = terminal_fixture();
        let record = records.records.first_mut().unwrap();
        assert!(matches_prompt(record, "session", "first-prompt"));
        finalize_record(record, true, "end", Some("session"), None);
        assert!(!matches_prompt(record, "session", "ordinary-followup"));
        assert!(!matches_prompt(record, "session", ""));
        record.prompt_id = None; // Backward-compatible old records cannot claim a new prompt.
        assert!(!matches_prompt(record, "session", "ordinary-followup"));
        assert_eq!(fact.prompt_id.as_deref(), Some("first-prompt"));
    }

    #[test]
    fn terminal_retry_keeps_original_result_through_two_io_failures_and_restart() {
        let (records, mut fact) = terminal_fixture();
        let mut disk_history = serde_json::to_string(&records).unwrap();
        let mut disk_outbox = String::new();
        for attempt in 0..3 {
            let mut loaded: RunRecordStore = serde_json::from_str(&disk_history).unwrap();
            let outcome = persist_terminal_transition(
                &mut loaded,
                &mut fact,
                |fact| {
                    disk_outbox = serde_json::to_string(fact).unwrap();
                    Ok(())
                },
                |records| {
                    if attempt < 2 {
                        return Err("injected history I/O failure".into());
                    }
                    disk_history = serde_json::to_string(records).unwrap();
                    Ok(())
                },
            );
            assert_eq!(outcome.is_ok(), attempt == 2);
            // Simulate losing all in-memory state between each attempt.
            fact = serde_json::from_str(&disk_outbox).unwrap();
            assert!(fact.ok);
            assert_eq!(fact.finished_at, "end");
        }
        let mut recovered: RunRecordStore = serde_json::from_str(&disk_history).unwrap();
        assert_eq!(recovered.records[0].status, "success");
        // Crash after history write but before local inbox: the outbox still
        // contains publication metadata, and replay does not rewrite execution.
        assert!(fact.completion.is_some());
        persist_terminal_transition(
            &mut recovered,
            &mut fact,
            |_| Ok(()),
            |_| panic!("terminal execution must not be rewritten"),
        )
        .unwrap();
        assert_eq!(fact.completion.as_ref().unwrap().event.status, "success");
        assert!(!apply_terminal_fact(&mut recovered, &fact).unwrap());
    }

    #[test]
    fn external_delivery_requires_durable_claim_and_restart_does_not_blindly_send_again() {
        let (_, mut fact) = terminal_fixture();
        let mut receipt = String::new();
        assert!(claim_external_attempt(&mut fact, true, |fact| {
            receipt = serde_json::to_string(fact).unwrap();
            Ok(())
        })
        .unwrap());
        let mut restored: TerminalFact = serde_json::from_str(&receipt).unwrap();
        assert!(restored.external_result.is_none()); // Unknown, never treated as delivered.
        assert!(!claim_external_attempt(&mut restored, true, |_| Ok(())).unwrap());
        let (_, mut unclaimed) = terminal_fixture();
        assert!(
            claim_external_attempt(&mut unclaimed, true, |_| Err("disk unavailable".into()))
                .is_err()
        );
    }

    #[test]
    fn terminal_receipt_paths_are_fixed_length_even_for_legacy_record_ids() {
        let long = "legacy/".repeat(30);
        let path = terminal_path(&long).unwrap();
        assert_eq!(path.file_name().unwrap().to_string_lossy().len(), 69);
        assert_eq!(path.parent().unwrap(), terminal_dir());
        assert_ne!(
            terminal_path("id-a").unwrap(),
            terminal_path("id-b").unwrap()
        );
    }
    #[test]
    fn automation_expert_legacy_reference_requires_reselection_and_path_survives_json() {
        let mut automation = test_automation();
        automation.expert_id = Some("old-name".into());
        automation.expert_name = Some("old-name".into());
        assert!(selected_expert_identity(&automation)
            .unwrap_err()
            .contains("重新选择"));
        let legacy = serde_json::to_value(&automation).unwrap();
        let mut legacy_object = legacy.as_object().unwrap().clone();
        legacy_object.remove("expertPath");
        let loaded: Automation =
            serde_json::from_value(serde_json::Value::Object(legacy_object)).unwrap();
        assert!(loaded.expert_path.is_none());
        assert!(resolve_selected_expert_prompt(&loaded, Path::new("/fixture-unused")).is_err());
        automation.expert_path = Some("/stable/definition.md".into());
        let loaded: Automation =
            serde_json::from_str(&serde_json::to_string(&automation).unwrap()).unwrap();
        assert_eq!(
            selected_expert_identity(&loaded).unwrap(),
            Some(("old-name", "/stable/definition.md"))
        );
    }

    #[test]
    fn automation_expert_rename_and_reused_name_cannot_rebind_selected_path() {
        let workspace = tempfile::tempdir().unwrap();
        let cwd = workspace.path().canonicalize().unwrap();
        let agents = cwd.join(".echo-agent/agents");
        std::fs::create_dir_all(&agents).unwrap();
        let selected_path = agents.join("stable.md");
        let raw = crate::agents_store::agents_template(
            "automation-stable-fixture".into(),
            "fixture".into(),
            "Original role".into(),
            None,
            None,
        )
        .unwrap();
        std::fs::write(&selected_path, &raw).unwrap();
        let mut automation = test_automation();
        automation.expert_name = Some("automation-stable-fixture".into());
        automation.expert_id = automation.expert_name.clone();
        automation.expert_path = Some(selected_path.to_string_lossy().into_owned());
        assert_eq!(
            resolve_selected_expert_prompt(&automation, &cwd)
                .unwrap()
                .as_deref(),
            Some("Original role")
        );
        std::fs::write(
            &selected_path,
            raw.replace("automation-stable-fixture", "automation-renamed-fixture"),
        )
        .unwrap();
        std::fs::write(
            agents.join("replacement.md"),
            raw.replace("Original role", "Other role"),
        )
        .unwrap();
        assert!(resolve_selected_expert_prompt(&automation, &cwd).is_err());
        // Explicit reselection of the renamed same file is allowed.
        automation.expert_name = Some("automation-renamed-fixture".into());
        automation.expert_id = automation.expert_name.clone();
        assert_eq!(
            resolve_selected_expert_prompt(&automation, &cwd)
                .unwrap()
                .as_deref(),
            Some("Original role")
        );
    }
}
