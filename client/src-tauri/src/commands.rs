//! Tauri command table — the frontend↔Rust contract.
//!
//! Commands are declared with `#[tauri::command]` and registered in lib.rs.
//! They drive the in-process EchoAgent runtime (see agent_runtime.rs) and bridge streamed
//! events back via Tauri events (see bridge.rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Emitter, Manager, State};

use crate::agent_runtime::{self, AgentHandle, InitOutcome};
use crate::bridge::{
    FolderTrusts, PendingInteractionsFrontend, PermissionOutcome, Permissions, PlanApprovalOutcome,
    PlanApprovals, QuestionOutcome, Questions,
};
use crate::sessions::{self, SessionSummary, WorkspaceInfo};

const AGENT_INITIALIZE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const NEW_SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
const MODEL_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MODEL_RELOAD_ATTEMPTS: usize = 3;
const MAX_SESSION_ID_CHARS: usize = 256;
const MAX_PROMPT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DISPLAY_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUESTION_FIELDS: usize = 128;
const MAX_QUESTION_KEY_CHARS: usize = 16 * 1024;
const MAX_QUESTION_VALUE_CHARS: usize = 64 * 1024;
const MAX_QUESTION_SELECTIONS: usize = 64;
const MAX_QUESTION_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

fn valid_session_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_SESSION_ID_CHARS
        && !value.chars().any(char::is_control)
}

fn validate_send_payload(
    session_id: &str,
    text: &str,
    display_text: Option<&str>,
) -> Result<(), String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err("消息内容不能超过 4 MiB".into());
    }
    if display_text.is_some_and(|value| value.len() > MAX_DISPLAY_TEXT_BYTES) {
        return Err("展示文本不能超过 4 MiB".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeModelState {
    initialized: bool,
    revision: Option<String>,
    model_ids: Vec<String>,
    /// Authentication method reconciled from the same Runtime model reload.
    /// A catalog without this acknowledgement is not usable by `session/new`.
    auth_method_id: Option<String>,
    last_error: Option<String>,
    /// Sender identity that produced this snapshot. A late response from a
    /// retired Runtime must never mark a newly-started Runtime as ready.
    sender: Option<echo_agent_acp::AcpAgentTx>,
}

/// State held across commands. The agent channel endpoints live here once
/// `agent_init` has spawned the agent.
#[derive(Default)]
pub struct AppState {
    pub handle: Mutex<Option<AgentHandle>>,
    /// Once the dispatcher owns the rx half, only the tx is reachable. We
    /// stash a clone of the tx sender here for commands to use.
    pub tx: Mutex<Option<echo_agent_acp::AcpAgentTx>>,
    pub cwd: Mutex<Option<PathBuf>>,
    /// Background automation scheduler bound to the current agent runtime.
    /// Replacing/aborting it on restart prevents a stale task from retaining a
    /// closed ACP sender forever.
    pub automation_scheduler: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Last model configuration positively acknowledged by the embedded
    /// Runtime. Disk configuration alone must never unlock message sending.
    pub(crate) runtime_models: Mutex<RuntimeModelState>,
    /// A synchronous readiness check may be the first observer of an expired
    /// organization-model lease. Remember that disk mutation until an
    /// app-aware caller can start the serialized Runtime hot reload.
    runtime_model_reload_pending: std::sync::atomic::AtomicBool,
    /// Successful ACP initialize response for the installed Runtime. A WebView
    /// reload reuses this generation instead of cancelling unattended work.
    pub(crate) init_outcome: Mutex<Option<InitOutcome>>,
    /// Serialize Runtime model reloads. Organization sync, settings edits and
    /// startup can all request a reload at the same time; overlapping ACP
    /// reloads make it impossible to know which disk revision was applied.
    pub(crate) model_reload_lock: tokio::sync::Mutex<()>,
    /// Serialize `agent_init`. The command is reachable from startup, the retry
    /// button and the settings dialog at once; interleaved runs would let an
    /// earlier call's failure cleanup tear down a later, healthy Runtime.
    pub(crate) init_lock: tokio::sync::Mutex<()>,
    /// Monotonic initialization generation. Cleanup paths and lifecycle events
    /// carry the generation that produced them so a retired Runtime can never
    /// invalidate its replacement.
    pub(crate) init_generation: std::sync::atomic::AtomicU64,
    /// Sessions created by a `new_session` request whose caller had already
    /// timed out. The Runtime completes and persists these regardless, so they
    /// are tracked here and reclaimed instead of leaking as ghost sessions.
    pub(crate) orphaned_sessions: Mutex<Vec<OrphanedSession>>,
    session_creations: crate::session_creation::SessionCreations,
    /// Native-authoritative workspace binding for every session admitted into
    /// the current Runtime generation. Renderer-provided session ids and paths
    /// are never sufficient to authorize attachment reads.
    session_workspaces: Mutex<HashMap<String, PathBuf>>,
}

/// A session the Runtime finished creating after its caller stopped waiting.
/// `cwd`, model and permission mode are kept so a retry only adopts a session
/// that matches what it would have asked for.
#[derive(Debug, Clone)]
pub(crate) struct OrphanedSession {
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) model_id: Option<String>,
    pub(crate) permission_mode: String,
}

impl AppState {
    /// Claim the next initialization generation. Every later state mutation from
    /// that `agent_init` call is gated on the claimed value still being current.
    pub(crate) fn begin_init_generation(&self) -> u64 {
        self.init_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    pub(crate) fn is_current_generation(&self, generation: u64) -> bool {
        self.init_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            == generation
    }

    pub(crate) fn is_runtime_current(&self, sender: &echo_agent_acp::AcpAgentTx) -> bool {
        self.tx
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
    }

    /// Record a session that arrived after its caller stopped waiting. The
    /// Runtime already persisted it, so the id is kept for reclamation.
    pub(crate) fn record_orphaned_session(&self, orphan: OrphanedSession) {
        let mut orphaned = self.orphaned_sessions.lock().unwrap();
        if !orphaned
            .iter()
            .any(|existing| existing.session_id == orphan.session_id)
        {
            orphaned.push(orphan);
        }
    }

    /// Adopt a previously orphaned session that matches the request a caller is
    /// about to make, so a retry after a timeout reuses the session the Runtime
    /// already created instead of leaving it behind and making another.
    pub(crate) fn take_orphaned_session(
        &self,
        cwd: &str,
        model_id: Option<&str>,
        permission_mode: &str,
    ) -> Option<String> {
        let mut orphaned = self.orphaned_sessions.lock().unwrap();
        let index = orphaned.iter().position(|candidate| {
            candidate.cwd == cwd
                && candidate.model_id.as_deref() == model_id
                && candidate.permission_mode == permission_mode
        })?;
        Some(orphaned.remove(index).session_id)
    }

    pub(crate) fn clear_orphaned_sessions(&self) {
        self.orphaned_sessions.lock().unwrap().clear();
        self.session_creations.clear();
    }

    pub(crate) fn record_session_workspace(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<(), String> {
        crate::meta::bind_session_workspace(session_id, cwd)?;
        self.session_workspaces
            .lock()
            .unwrap()
            .insert(session_id.to_string(), cwd.to_path_buf());
        Ok(())
    }

    pub(crate) fn session_workspace(&self, session_id: &str) -> Result<PathBuf, String> {
        self.session_workspaces
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "当前会话尚未建立可信工作区绑定，请重新打开会话".to_string())
    }

    pub(crate) fn session_ids(&self) -> Vec<String> {
        self.session_workspaces
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn forget_session_workspace(&self, session_id: &str) {
        self.session_workspaces.lock().unwrap().remove(session_id);
    }

    pub(crate) fn clear_session_workspaces(&self) {
        self.session_workspaces.lock().unwrap().clear();
    }

    pub(crate) fn mark_runtime_models_initializing(&self) {
        *self.runtime_models.lock().unwrap() = RuntimeModelState::default();
    }

    fn mark_runtime_model_reload_pending(&self) {
        self.mark_runtime_models_initializing();
        self.runtime_model_reload_pending
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn take_runtime_model_reload_pending(&self) -> bool {
        self.runtime_model_reload_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn mark_runtime_models_initializing_if_current(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
    ) -> bool {
        let tx = self.tx.lock().unwrap();
        if !tx
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            return false;
        }
        *self.runtime_models.lock().unwrap() = RuntimeModelState::default();
        true
    }

    pub(crate) fn mark_runtime_models_synced(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
        revision: String,
        model_ids: Vec<String>,
        auth_method_id: String,
    ) -> bool {
        let current_sender = self.tx.lock().unwrap();
        if !current_sender
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            return false;
        }
        *self.runtime_models.lock().unwrap() = RuntimeModelState {
            initialized: true,
            revision: Some(revision),
            model_ids,
            auth_method_id: Some(auth_method_id),
            last_error: None,
            sender: Some(sender.clone()),
        };
        true
    }

    pub(crate) fn mark_runtime_models_failed(&self, error: impl Into<String>) {
        let mut runtime = self.runtime_models.lock().unwrap();
        runtime.initialized = false;
        runtime.revision = None;
        runtime.model_ids.clear();
        runtime.auth_method_id = None;
        runtime.sender = None;
        runtime.last_error = Some(error.into());
    }

    pub(crate) fn mark_runtime_models_failed_if_current(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
        error: impl Into<String>,
    ) {
        let current_sender = self.tx.lock().unwrap();
        if current_sender
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            self.mark_runtime_models_failed(error);
        }
    }

    /// Invalidate the command-facing runtime only when `tx` is still the
    /// currently installed agent. An older agent can finish joining after a
    /// user has already restarted a replacement; it must not poison the new
    /// runtime's readiness state.
    /// Returns whether the teardown applied. Callers use the result to decide
    /// whether to emit a user-visible "agent died" event: an event from a
    /// retired Runtime would make the frontend mark its healthy replacement
    /// unavailable.
    pub(crate) fn mark_runtime_dead_if_current(
        &self,
        tx: &echo_agent_acp::AcpAgentTx,
        error: impl Into<String>,
    ) -> bool {
        let mut current_tx = self.tx.lock().unwrap();
        let is_current = current_tx
            .as_ref()
            .is_some_and(|current| current.same_channel(tx));
        if !is_current {
            return false;
        }
        current_tx.take();
        // Keep the sender guard through the synchronous state changes so an
        // initialization cannot publish a replacement between identity check
        // and teardown of its handle/readiness.
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.cancel.cancel();
        }
        if let Some(scheduler) = self.automation_scheduler.lock().unwrap().take() {
            scheduler.abort();
        }
        crate::automations::clear_runtime_sessions();
        self.clear_orphaned_sessions();
        self.clear_session_workspaces();
        crate::agent_admin::clear_runtime_capabilities();
        self.init_outcome.lock().unwrap().take();
        self.mark_runtime_models_failed(error);
        crate::permission_config::mark_runtime_permission_mode_offline(None);
        true
    }

    /// Remove a dead gateway from readiness immediately, while keeping its
    /// worker handle and LocalSet available for final persistence/retry.
    fn quarantine_gateway_if_current(
        &self,
        tx: &echo_agent_acp::AcpAgentTx,
        error: String,
    ) -> bool {
        let mut current_tx = self.tx.lock().unwrap();
        if !current_tx
            .as_ref()
            .is_some_and(|current| current.same_channel(tx))
        {
            return false;
        }
        current_tx.take();
        // Match the same sender throughout quarantine, including its handle
        // and readiness. Publishing another sender must wait for this guard.
        if let Some(handle) = self.handle.lock().unwrap().as_ref() {
            handle.cancel.cancel();
        }
        if let Some(scheduler) = self.automation_scheduler.lock().unwrap().take() {
            scheduler.abort();
        }
        self.init_outcome.lock().unwrap().take();
        self.mark_runtime_models_failed(error);
        crate::permission_config::mark_runtime_permission_mode_offline(None);
        true
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub ready: bool,
    pub reason: Option<String>,
    /// Model ids configured in `~/.echo-agent/config.toml`. When non-empty the
    /// app can route prompts to the matching `[model.*]` backend.
    pub providers: Vec<String>,
    /// True only when the embedded Runtime has acknowledged the exact
    /// model-related configuration revision currently on disk.
    pub runtime_ready: bool,
    /// True only when the Runtime also installed an authentication method for
    /// the acknowledged model configuration.
    pub runtime_auth_ready: bool,
    pub synchronized: bool,
    /// Runtime catalog as shown to the frontend: upstream-branded ids removed.
    pub runtime_models: Vec<String>,
    /// Concrete configured model used when a caller requests automatic model
    /// selection. Unlike the Runtime's bundled fallback, this id is guaranteed
    /// to exist in both the user's usable configuration and the live catalog.
    pub default_model_id: Option<String>,
    pub last_runtime_error: Option<String>,
    /// The Runtime's catalog verbatim, used for authorization decisions only.
    ///
    /// Not serialized: the frontend must render `runtime_models`. Gate checks
    /// (`validate_runtime_ready`) read this instead, so the display filter can
    /// never refuse a model the Runtime actually loaded — including a user's own
    /// connection whose id happens to contain a brand token.
    #[serde(skip)]
    pub unfiltered_runtime_models: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitResult {
    pub ok: bool,
    pub auth: AuthStatus,
    pub cwd: String,
    pub agent_version: Option<String>,
    pub default_model_id: Option<String>,
    pub build_commit: String,
    /// Wall-clock time this binary was compiled.
    pub build_time: String,
    /// Commit date of `build_commit`. Kept distinct from `build_time`: the two
    /// diverge whenever a commit is built later than it was authored.
    pub build_commit_time: String,
    pub log_dir: String,
}

fn init_result(cwd: &Path, auth: AuthStatus, outcome: &InitOutcome) -> InitResult {
    InitResult {
        ok: true,
        auth,
        cwd: cwd.to_string_lossy().into_owned(),
        agent_version: outcome.agent_version.clone(),
        default_model_id: outcome.default_model_id.clone(),
        build_commit: env!("ECHOAGENT_BUILD_COMMIT").to_string(),
        build_time: env!("ECHOAGENT_BUILD_TIME").to_string(),
        build_commit_time: env!("ECHOAGENT_BUILD_COMMIT_TIME").to_string(),
        log_dir: crate::logging::log_dir().to_string_lossy().into_owned(),
    }
}

fn auth_status(state: &AppState) -> AuthStatus {
    match crate::org::enforce_organization_model_lease() {
        Ok(true) => state.mark_runtime_model_reload_pending(),
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, "failed to enforce organization model lease"),
    }
    let (mut model_ids, disk_reason) = crate::providers::usable_model_ids();
    model_ids.sort();
    let revision = crate::providers::model_config_revision();
    let (runtime, sender_current) = {
        let current_sender = state.tx.lock().unwrap();
        let runtime = state.runtime_models.lock().unwrap().clone();
        let sender_current = runtime.sender.as_ref().is_some_and(|snapshot_sender| {
            current_sender
                .as_ref()
                .is_some_and(|current| current.same_channel(snapshot_sender))
        });
        (runtime, sender_current)
    };
    let preferred = crate::permission_config::read_defaults().default_model;
    let visible_model_ids = strip_upstream_branded_ids(model_ids.clone());
    let visible_runtime_models = strip_upstream_branded_ids(runtime.model_ids.clone());
    let default_model_id = effective_configured_model_id(
        &visible_model_ids,
        &visible_runtime_models,
        (!preferred.trim().is_empty()).then_some(preferred.as_str()),
    );
    let mut status =
        auth_status_from_snapshots(model_ids, disk_reason, &revision, runtime, sender_current);
    status.default_model_id = default_model_id;
    status
}

fn auth_status_with_runtime_reload(app: &tauri::AppHandle, state: &AppState) -> AuthStatus {
    let status = auth_status(state);
    if state.take_runtime_model_reload_pending() {
        crate::org::notify_models_changed(app, "lease-expired-readiness-check");
    }
    status
}

/// Resolve Auto to a live configured model. An explicit user preference wins;
/// otherwise the organization model, a personal model, then chat-xc wins.
fn effective_configured_model_id(
    configured: &[String],
    runtime: &[String],
    preferred: Option<&str>,
) -> Option<String> {
    let available = |id: &str| {
        configured.iter().any(|configured_id| configured_id == id)
            && runtime.iter().any(|runtime_id| runtime_id == id)
    };
    preferred
        .filter(|id| available(id))
        .map(str::to_string)
        .or_else(|| {
            configured
                .iter()
                .find(|id| id.starts_with("organization/") && available(id))
                .cloned()
        })
        .or_else(|| {
            configured
                .iter()
                .find(|id| {
                    !id.starts_with("organization/")
                        && !id.starts_with(crate::providers::BUILTIN_MODEL_PREFIX)
                        && available(id)
                })
                .cloned()
        })
        .or_else(|| {
            available(crate::providers::BUILTIN_DEFAULT_MODEL_ID)
                .then(|| crate::providers::BUILTIN_DEFAULT_MODEL_ID.to_string())
        })
        .or_else(|| configured.iter().find(|id| available(id)).cloned())
}

fn auth_status_from_snapshots(
    model_ids: Vec<String>,
    disk_reason: Option<String>,
    revision: &str,
    runtime: RuntimeModelState,
    sender_current: bool,
) -> AuthStatus {
    // The Runtime's effective catalog is the only authority on which models can
    // actually serve a prompt. `[models]` filters (allowed_models,
    // hidden_models, disabled_models) are applied inside the Runtime, so a disk
    // entry legitimately may not appear in the catalog. Requiring disk ⊆ runtime
    // would lock chat permanently for a correct configuration that hides or
    // disables a single model.
    let synchronized =
        sender_current && runtime.initialized && runtime.revision.as_deref() == Some(revision);
    let runtime_auth_ready = synchronized && runtime.auth_method_id.is_some();
    let runtime_ready = synchronized
        && runtime.last_error.is_none()
        && !runtime.model_ids.is_empty()
        && runtime_auth_ready;
    let ready = runtime_ready;
    let reason = if ready {
        None
    } else if model_ids.is_empty() {
        disk_reason
    } else if let Some(error) = runtime.last_error.clone() {
        Some(format!("Agent Runtime 模型刷新失败：{error}"))
    } else if !runtime.initialized {
        Some("Agent Runtime 尚未完成初始化，请稍候或重启应用。".into())
    } else if !synchronized {
        Some("Agent Runtime 中的模型配置与磁盘不一致，请在设置中重试刷新。".into())
    } else if !runtime_auth_ready {
        Some(
            "Agent Runtime 尚未完成模型凭证同步，请稍候后重试；若持续出现，请刷新模型配置。".into(),
        )
    } else {
        Some("Agent Runtime 未加载任何可用模型，请检查“设置 → 模型与连接”中的可用范围配置。".into())
    };
    // Branded-id filtering applies ONLY to the two fields the frontend renders.
    // Every gate above (`synchronized`, `runtime_ready`, `ready`, `reason`) is
    // computed from the unfiltered catalog on purpose: an empty filtered list
    // must never flip `ready` to false and lock chat. `require_runtime_ready`
    // likewise validates a requested model against the unfiltered catalog, so a
    // legitimate request is never refused because of a display filter.
    //
    // For a correctly isolated BYOK setup this is a no-op — the user's own model
    // ids carry no upstream brand. It matters when the source-level isolation in
    // `agent_runtime` fails (e.g. an upstream upgrade renames the config keys it
    // depends on): the UI stays clean instead of surfacing bundled model ids.
    AuthStatus {
        ready,
        reason,
        providers: strip_upstream_branded_ids(model_ids),
        runtime_ready,
        runtime_auth_ready,
        synchronized,
        runtime_models: strip_upstream_branded_ids(runtime.model_ids.clone()),
        default_model_id: None,
        last_runtime_error: runtime.last_error,
        unfiltered_runtime_models: runtime.model_ids,
    }
}

/// Upstream vendor brand tokens that must not reach the desktop UI.
///
/// Kept in lockstep with `src/lib/model-branding.ts` — the frontend filters the
/// same tokens as a second layer for values that do not flow through here (usage
/// records, transcript model dividers).
// Retired upstream identifiers remain display-filter inputs for legacy model
// catalogs. Build them from fragments so they cannot be mistaken for current
// EchoAgent product names during source audits.
const UPSTREAM_BRAND_TOKENS: &[&str] = &[
    concat!("g", "rok"),
    concat!("x", "ai"),
    concat!("x", ".", "ai"),
    concat!("space", "x", "ai"),
];

fn is_upstream_branded_model_id(id: &str) -> bool {
    let normalized = id.to_ascii_lowercase();
    UPSTREAM_BRAND_TOKENS
        .iter()
        .any(|token| normalized.contains(token))
}

fn strip_upstream_branded_ids(ids: Vec<String>) -> Vec<String> {
    ids.into_iter()
        .filter(|id| !is_upstream_branded_model_id(id))
        .collect()
}

/// Admit a command only after the in-process Runtime has acknowledged the same
/// model configuration that is currently on disk. This is deliberately checked
/// in Rust as well as in the Composer: queue/plan/automation callers can invoke
/// `agent_send` without going through the visible send button.
pub(crate) fn require_runtime_ready(
    app: Option<&tauri::AppHandle>,
    state: &AppState,
    requested_model: Option<&str>,
) -> Result<(), String> {
    let status = app
        .map(|app| auth_status_with_runtime_reload(app, state))
        .unwrap_or_else(|| auth_status(state));
    validate_runtime_ready(&status, requested_model)
}

/// Return the concrete model that must be bound to a new automation session.
/// `None` means Auto at the storage/UI boundary, but is never forwarded as None
/// to the Runtime because that would select its internal fallback model.
pub(crate) fn resolve_automation_model_id(
    app: Option<&tauri::AppHandle>,
    state: &AppState,
    requested_model: Option<&str>,
) -> Result<String, String> {
    let status = app
        .map(|app| auth_status_with_runtime_reload(app, state))
        .unwrap_or_else(|| auth_status(state));
    validate_runtime_ready(&status, requested_model)?;
    if let Some(model_id) = requested_model {
        let (configured, _) = crate::providers::usable_model_ids();
        if !configured
            .iter()
            .any(|configured_id| configured_id == model_id)
        {
            return Err(format!(
                "自动化任务选择的模型“{model_id}”已不在可用配置中，请重新选择模型后保存。"
            ));
        }
        return Ok(model_id.to_string());
    }
    status
        .default_model_id
        .ok_or_else(|| "自动化任务没有可用的默认模型，请先在“设置 → 模型与连接”配置模型。".into())
}

fn validate_runtime_ready(
    status: &AuthStatus,
    requested_model: Option<&str>,
) -> Result<(), String> {
    if !status.ready {
        let reason = status
            .reason
            .clone()
            .unwrap_or_else(|| "Agent Runtime 模型未就绪，请在设置中重试模型配置。".into());
        tracing::warn!(%reason, requested_model, "agent command blocked by runtime readiness gate");
        return Err(reason);
    }
    if let Some(model_id) = requested_model {
        // Deliberately the unfiltered catalog: the display filter must not gate
        // what can be sent (see `AuthStatus::unfiltered_runtime_models`).
        if !status
            .unfiltered_runtime_models
            .iter()
            .any(|available| available == model_id)
        {
            let reason = format!("Agent Runtime 尚未加载模型“{model_id}”，请刷新模型配置后重试。");
            tracing::warn!(%reason, model_id, "agent command requested unavailable runtime model");
            return Err(reason);
        }
    }
    Ok(())
}

/// Read the model-config revision without accepting a torn snapshot while
/// config.toml is being atomically replaced. A caller can compare the returned
/// revision with the revision observed before the Runtime reload.
fn stable_model_revision() -> Option<String> {
    for _ in 0..4 {
        let before = crate::providers::model_config_revision();
        let after = crate::providers::model_config_revision();
        if before == after {
            return Some(after);
        }
        std::thread::yield_now();
    }
    None
}

/// Tear down a partially initialized runtime before returning an init error.
/// The dispatcher will naturally stop once its ACP channel is closed; clearing
/// the command-facing handles immediately prevents sends from hanging on a
/// stale channel during a retry.
/// Only the generation that owns the current runtime may tear it down; a stale
/// call returns without touching a newer, healthy runtime.
fn clear_runtime_after_init_failure(state: &AppState, generation: u64) {
    if !state.is_current_generation(generation) {
        tracing::warn!(
            generation,
            "skipping init failure cleanup for a superseded generation"
        );
        return;
    }
    if let Some(handle) = state.handle.lock().unwrap().take() {
        handle.cancel.cancel();
    }
    state.tx.lock().unwrap().take();
    if let Some(scheduler) = state.automation_scheduler.lock().unwrap().take() {
        scheduler.abort();
    }
    crate::automations::clear_runtime_sessions();
    state.clear_orphaned_sessions();
    state.clear_session_workspaces();
    crate::agent_admin::clear_runtime_capabilities();
    state.init_outcome.lock().unwrap().take();
    state.mark_runtime_models_initializing();
    crate::permission_config::mark_runtime_permission_mode_offline(Some(
        "Agent Runtime 初始化失败".into(),
    ));
}

/// Reload model configuration and acknowledge it only when the file stayed
/// unchanged for the complete Runtime round-trip. Settings and organization
/// sync can write config.toml while a reload is in flight; retrying closes that
/// race instead of advertising a stale Runtime as ready.
pub(crate) async fn reload_models_and_sync(
    app: &tauri::AppHandle,
    state: &AppState,
    tx: &echo_agent_acp::AcpAgentTx,
) -> Result<(), String> {
    let _reload_guard = state.model_reload_lock.lock().await;
    if !state.mark_runtime_models_initializing_if_current(tx) {
        return Err("Agent Runtime 已在模型刷新期间重启，请重试。".into());
    }
    let mut last_revision = None;
    for attempt in 1..=MODEL_RELOAD_ATTEMPTS {
        crate::providers::sync_insecure_http_approvals();
        let before = crate::providers::model_config_revision();
        let result = crate::agent_admin::request_model_reload_and_wait(tx).await;
        match result {
            Err(error) => {
                state.mark_runtime_models_failed_if_current(tx, error.clone());
                return Err(error);
            }
            Ok(ack) => {
                let after = crate::providers::model_config_revision();
                if before == after {
                    if let Some(revision) = stable_model_revision() {
                        if revision == before {
                            match tokio::time::timeout(
                                MODEL_CATALOG_TIMEOUT,
                                agent_runtime::model_ids(tx),
                            )
                            .await
                            {
                                Err(_) => {
                                    let error = format!(
                                        "读取 Agent Runtime 模型目录超时（{} 秒）",
                                        MODEL_CATALOG_TIMEOUT.as_secs()
                                    );
                                    state.mark_runtime_models_failed_if_current(tx, error.clone());
                                    return Err(error);
                                }
                                Ok(Ok(mut runtime_model_ids)) => {
                                    runtime_model_ids.sort();
                                    runtime_model_ids.dedup();
                                    // An empty catalog is the one unrecoverable
                                    // outcome: it means the Runtime rejected the
                                    // reload (invalid model filters are dropped
                                    // silently by `apply_config`, which still
                                    // answers success) or every model was
                                    // filtered out. Retrying cannot fix a
                                    // configuration error, so fail with an
                                    // actionable reason instead of looping.
                                    if runtime_model_ids.is_empty() {
                                        let error = "Agent Runtime 未加载任何可用模型；请检查 config.toml 中 [models] 的 allowed_models / hidden_models / disabled_models 配置。".to_string();
                                        tracing::error!(attempt, %error, "Runtime model catalog is empty after reload");
                                        state.mark_runtime_models_failed_if_current(
                                            tx,
                                            error.clone(),
                                        );
                                        return Err(error);
                                    }
                                    let Some(auth_method_id) = ack.auth_method_id else {
                                        let error = "Agent Runtime 已加载模型，但尚未完成凭证认证同步；请刷新模型配置后重试。".to_string();
                                        tracing::error!(
                                            attempt,
                                            runtime_models = runtime_model_ids.len(),
                                            acknowledged_models = ack.model_count,
                                            %error,
                                            "Runtime auth method is missing after model reload"
                                        );
                                        state.mark_runtime_models_failed_if_current(
                                            tx,
                                            error.clone(),
                                        );
                                        return Err(error);
                                    };
                                    let committed = state.mark_runtime_models_synced(
                                        tx,
                                        revision,
                                        runtime_model_ids,
                                        auth_method_id,
                                    );
                                    if !committed {
                                        return Err(
                                            "Agent Runtime 已在模型刷新期间重启，请重试。".into()
                                        );
                                    }
                                    let _ = app.emit(
                                        "agent://models-update",
                                        serde_json::json!({ "source": "runtime-ready" }),
                                    );
                                    return Ok(());
                                }
                                Ok(Err(error)) => {
                                    state.mark_runtime_models_failed_if_current(
                                        tx,
                                        error.to_string(),
                                    );
                                    return Err(error.to_string());
                                }
                            }
                        }
                    }
                }
                last_revision = Some(after);
                tracing::warn!(
                    attempt,
                    "model config changed while Runtime reload was in flight; retrying"
                );
            }
        }
    }

    let error = format!(
        "模型配置在 Runtime 刷新期间持续变化（{} 次），发送功能已保持禁用，请重试。",
        MODEL_RELOAD_ATTEMPTS
    );
    tracing::error!(revision = ?last_revision, %error, "model Runtime reload did not converge");
    state.mark_runtime_models_failed_if_current(tx, error.clone());
    Err(error)
}

fn default_cwd() -> Result<PathBuf, String> {
    crate::paths::default_workspace_dir()
}

/// Validate, but never create, a session workspace capability. Only native
/// folder selection, the native default workspace, or trusted persisted roots
/// may populate `FilesystemAccess`; renderer calls cannot self-authorize by
/// reaching the session lifecycle API.
fn authorized_session_cwd(
    filesystem: &crate::shell_fs::FilesystemAccess,
    claimed: &str,
) -> Result<String, String> {
    let path = filesystem.require_workspace(claimed)?;
    // Opening the directory checks access without reading any entries.
    std::fs::read_dir(&path).map_err(|error| format!("无法访问工作目录：{error}"))?;
    Ok(path.to_string_lossy().into_owned())
}

pub(crate) fn trusted_existing_session_cwd(
    state: &AppState,
    filesystem: &crate::shell_fs::FilesystemAccess,
    session_id: &str,
    claimed: Option<&str>,
) -> Result<PathBuf, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if let Some(bound) = state
        .session_workspace(session_id)
        .ok()
        .or_else(|| crate::meta::session_workspace(session_id))
    {
        if let Some(claimed) = claimed.filter(|value| !value.trim().is_empty()) {
            let claimed = filesystem.require_workspace(claimed)?;
            if claimed != bound {
                return Err("会话工作区与后端绑定不一致".into());
            }
        }
        let canonical = filesystem.require_workspace(&bound.to_string_lossy())?;
        if canonical != bound {
            return Err("原工作目录已被链接到其他位置，请恢复原目录或新建任务".into());
        }
        std::fs::read_dir(&bound).map_err(|error| format!("无法访问原工作目录：{error}"))?;
        return Ok(bound);
    }
    let claimed = claimed
        .filter(|value| !value.trim().is_empty())
        .ok_or("历史会话操作必须提供工作区")?;
    let canonical = filesystem.require_workspace(claimed)?;
    std::fs::read_dir(&canonical).map_err(|error| format!("无法访问原工作目录：{error}"))?;
    let canonical_text = canonical.to_string_lossy();
    if !sessions::list_sessions(&canonical_text, true)
        .iter()
        .any(|summary| summary.session_id == session_id)
    {
        return Err("会话不属于声明的工作区".into());
    }
    Ok(canonical)
}

/// History belongs to the recorded session even when its execution directory
/// is offline. A renderer-supplied path can narrow this identity, never replace it.
fn trusted_session_history_cwd(
    state: &AppState,
    session_id: &str,
    claimed: Option<&str>,
) -> Result<PathBuf, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    let bound = state
        .session_workspace(session_id)
        .ok()
        .or_else(|| crate::meta::session_workspace(session_id));
    let original = match bound {
        Some(path) => path,
        None => sessions::list_all_sessions(true)?
            .into_iter()
            .find(|session| session.session_id == session_id)
            .map(|session| PathBuf::from(session.cwd))
            .ok_or("会话不存在或已被删除")?,
    };
    if !original.is_absolute() {
        return Err("会话原工作区路径无效".into());
    }
    if let Some(claimed) = claimed.filter(|value| !value.trim().is_empty()) {
        let claimed = Path::new(claimed);
        if claimed != original && claimed.canonicalize().ok().as_deref() != Some(original.as_path())
        {
            return Err("会话工作区与后端绑定不一致".into());
        }
    }
    Ok(original)
}

/// Initialize the in-process EchoAgent runtime. Spawns the agent thread, runs
/// `initialize`, and starts the dispatcher.
#[tauri::command]
pub async fn agent_init(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    permissions: State<'_, Permissions>,
    questions: State<'_, Questions>,
    plan_approvals: State<'_, PlanApprovals>,
    folder_trusts: State<'_, FolderTrusts>,
    cwd: Option<String>,
) -> Result<InitResult, String> {
    // Startup, the retry button and the settings dialog can all call this at
    // once. Serializing the whole lifecycle keeps one initialization's spawn,
    // handshake and cleanup from interleaving with another's.
    let _init_guard = state.init_lock.lock().await;
    let _permission_transition_guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;

    // Permission choices are task-owned. Remove obsolete global preference
    // keys before launch so no Runtime fallback can broaden another task.
    crate::permission_config::clear_global_permission_mode()?;

    // Renderer reloads recreate the React tree but not the native process. Keep
    // the healthy Runtime and its in-flight automations alive; the dispatcher
    // already emits Tauri events independently of any WebView subscription.
    let reusable = {
        let tx = state.tx.lock().unwrap().clone();
        let outcome = state.init_outcome.lock().unwrap().clone();
        let existing_cwd = state.cwd.lock().unwrap().clone();
        match (tx, outcome, existing_cwd) {
            (Some(_), Some(outcome), Some(existing_cwd)) => {
                let same_cwd = match cwd.as_deref() {
                    None => true,
                    Some(requested) => {
                        app.state::<crate::shell_fs::FilesystemAccess>()
                            .require_workspace(requested)?
                            == existing_cwd
                    }
                };
                same_cwd.then_some((outcome, existing_cwd))
            }
            _ => None,
        }
    };
    if let Some((outcome, existing_cwd)) = reusable {
        if crate::permission_config::runtime_permission_mode_is_current() {
            tracing::info!("agent init reused the active Runtime");
            return Ok(init_result(
                &existing_cwd,
                auth_status_with_runtime_reload(&app, &state),
                &outcome,
            ));
        }
        tracing::warn!("active Runtime permission mode is stale; restarting instead of reusing");
    }

    // A genuinely new Runtime cannot still be executing work owned by the
    // previous process. Reconcile volatile persisted badges before exposing
    // the catalog; a renderer reload that reuses the live Runtime returned
    // above and deliberately keeps its in-flight states.
    if let Err(error) = crate::meta::mark_transient_statuses_stopped() {
        tracing::warn!(%error, "failed to reconcile persisted session lifecycle states");
    }

    let generation = state.begin_init_generation();

    // A renderer/runtime restart invalidates every outstanding reverse
    // request. Resolve all of them conservatively before installing the new
    // runtime so an old request can never approve work in a new generation.
    let (closed_permissions, _, _, _) = tokio::join!(
        permissions.cancel_all(),
        questions.cancel_all(),
        plan_approvals.cancel_all(),
        folder_trusts.cancel_all(),
    );
    for notice in closed_permissions {
        crate::bridge::emit_permission_closed(&app, notice);
    }

    // A retry/re-init must retire the prior runtime and its scheduler before a
    // replacement is spawned; dropping the handle alone does not cancel it.
    if let Some(scheduler) = state.automation_scheduler.lock().unwrap().take() {
        scheduler.abort();
    }
    stop_agent_runtime_wait(&state, RUNTIME_SHUTDOWN_GRACE).await?;
    crate::automations::clear_runtime_sessions();
    state.clear_orphaned_sessions();
    state.clear_session_workspaces();
    crate::agent_admin::clear_runtime_capabilities();
    state.init_outcome.lock().unwrap().take();
    state.mark_runtime_models_initializing();

    let cwd = match cwd {
        Some(cwd) => {
            let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
            filesystem.require_workspace(&cwd)?
        }
        None => {
            let cwd = default_cwd()?;
            let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
            filesystem.authorize_workspace(&cwd.to_string_lossy())?
        }
    };
    *state.cwd.lock().unwrap() = Some(cwd.clone());

    // Spawn the agent off the async runtime. `spawn_agent_runtime` does blocking I/O
    // (config load, first-run bundled extract under ~/.echo-agent). Running it
    // inline would stall Tauri's tokio workers and freeze the UI.
    let spawn_cwd = cwd.clone();
    let launch_permission_mode = crate::permission_config::read_permission_mode();
    let agent_runtime::AgentHandle {
        tx,
        rx,
        cancel,
        shutdown_complete,
        shutdown_failure,
        mut gateway_failure,
        thread,
    } = tokio::task::spawn_blocking(move || agent_runtime::spawn_agent_runtime(spawn_cwd))
        .await
        .map_err(|e| format!("spawn EchoAgent task: {e}"))?
        .map_err(|e| format!("spawn EchoAgent runtime: {e}"))?;
    crate::permission_config::mark_runtime_permission_mode_starting(&launch_permission_mode);

    // Stash tx for later commands; move rx into the dispatcher.
    *state.tx.lock().unwrap() = Some(tx.clone());

    // Start the dispatcher that forwards agent→client messages to events.
    // `rx` is moved in; the dispatcher owns it for the app lifetime.
    crate::bridge::spawn_dispatcher(
        app.clone(),
        rx,
        permissions.share(),
        questions.share(),
        plan_approvals.share(),
        folder_trusts.share(),
    );

    // Install the command-facing handle before starting the death monitor. A
    // Runtime can fail immediately after spawn; publishing the handle first
    // ensures that the monitor can invalidate the same generation instead of
    // racing with a later stale write from this initialization call.
    let (_placeholder_tx, placeholder_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    *state.handle.lock().unwrap() = Some(AgentHandle {
        tx: tx.clone(),
        // Unused placeholder rx — the real rx lives in the dispatcher.
        rx: placeholder_rx,
        cancel: cancel.clone(),
        shutdown_complete,
        shutdown_failure,
        gateway_failure: gateway_failure.clone(),
        thread: None,
    });

    // Joining the Runtime can legitimately wait for a failed disk write to be
    // repaired. Its gateway is already unusable during that wait and must not
    // remain eligible for init reuse or receive another user prompt.
    let gateway_app = app.clone();
    let gateway_tx = tx.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let failure = gateway_failure.borrow_and_update().clone();
            if let Some(reason) = failure {
                if gateway_app
                    .state::<AppState>()
                    .quarantine_gateway_if_current(&gateway_tx, reason.clone())
                {
                    let _ = gateway_app.emit(
                        "agent://agent-died",
                        serde_json::json!({ "reason": reason, "generation": generation }),
                    );
                }
                break;
            }
            if gateway_failure.changed().await.is_err() {
                break;
            }
        }
    });

    // Monitor the agent thread: if it exits unexpectedly (panic/crash), notify
    // the frontend so it can show a "restart agent" prompt instead of hanging.
    if let Some(join_handle) = thread {
        let monitor_app = app.clone();
        let monitor_cancel = cancel.clone();
        let monitor_tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = join_handle.join();
            // If the cancel token was triggered, this was an intentional shutdown — don't alarm.
            if monitor_cancel.is_cancelled() {
                return;
            }
            let reason = match result {
                Ok(Ok(())) => "agent thread exited normally (unexpected)".to_string(),
                Ok(Err(e)) => format!("agent error: {e}"),
                Err(_) => "agent thread panicked".to_string(),
            };
            tracing::error!(reason = %reason, generation, "EchoAgent runtime thread died");
            // Emit only when this runtime was still the installed one. A late
            // exit from a superseded runtime must not mark its replacement dead.
            let invalidated = monitor_app
                .state::<AppState>()
                .mark_runtime_dead_if_current(&monitor_tx, reason.clone());
            if !invalidated {
                tracing::info!(
                    generation,
                    "suppressing agent-died event from a superseded runtime"
                );
                return;
            }
            let _ = monitor_app.emit(
                "agent://agent-died",
                serde_json::json!({ "reason": reason, "generation": generation }),
            );
        });
    }

    // Run the ACP initialization lifecycle. Provider credentials are managed
    // solely through the model-provider configuration.
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent channel not ready")?;

    tracing::info!("agent initialize send");
    let init_outcome: InitOutcome = match tokio::time::timeout(
        AGENT_INITIALIZE_TIMEOUT,
        agent_runtime::initialize(&tx),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            let message = format!("initialize: {error}");
            tracing::error!(%message, "agent initialize failed");
            clear_runtime_after_init_failure(&state, generation);
            return Err(message);
        }
        Err(_) => {
            let message = "initialize timed out after 60 seconds".to_string();
            tracing::error!(%message, "agent initialize timed out");
            clear_runtime_after_init_failure(&state, generation);
            return Err(message);
        }
    };
    tracing::info!("agent initialize OK");
    crate::permission_config::mark_runtime_permission_mode_synced(&launch_permission_mode);

    // Close the startup race with settings/org writes: the Runtime explicitly
    // reloads the latest disk model config before it is advertised as ready.
    if let Err(error) = reload_models_and_sync(&app, &state, &tx).await {
        tracing::error!(%error, "initial model reload failed");
    }
    tracing::info!("initial model reload completed");
    *state.init_outcome.lock().unwrap() = Some(init_outcome.clone());
    let auth = auth_status_with_runtime_reload(&app, &state);
    tracing::info!(ready = auth.ready, "initial auth status resolved");

    // Start the automations scheduler now that the agent channel is up.
    // Idempotent — safe if agent_init is somehow called twice.
    if let (Some(tx), Some(cwd)) = (
        state.tx.lock().unwrap().clone(),
        state.cwd.lock().unwrap().clone(),
    ) {
        let scheduler = crate::automations::start_scheduler(app.clone(), tx, cwd);
        if let Some(previous) = state
            .automation_scheduler
            .lock()
            .unwrap()
            .replace(scheduler)
        {
            previous.abort();
        }
    }

    tracing::info!("agent initialization completed");
    Ok(init_result(&cwd, auth, &init_outcome))
}

#[tauri::command]
pub fn agent_auth_status(app: tauri::AppHandle, state: State<'_, AppState>) -> AuthStatus {
    auth_status_with_runtime_reload(&app, &state)
}

/// Only the native default root can allocate automatic task directories.
#[tauri::command]
pub fn agent_prepare_task_workspace(
    app: tauri::AppHandle,
    request_id: String,
) -> Result<String, String> {
    crate::policy::require_feature("sessions")?;
    let base = default_cwd()?;
    let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
    filesystem.require_workspace(&base.to_string_lossy())?;
    let directory = crate::session_workspace::prepare_task_directory(&base, &request_id)?;
    Ok(filesystem
        .require_workspace(&directory.to_string_lossy())?
        .to_string_lossy()
        .into_owned())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWorkspaceStatus {
    cwd: String,
    loaded: bool,
    status: &'static str,
    message: Option<String>,
}

#[tauri::command]
pub fn agent_session_workspace_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    cwd: String,
) -> Result<SessionWorkspaceStatus, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效".into());
    }
    let original = state
        .session_workspace(&session_id)
        .ok()
        .or_else(|| crate::meta::session_workspace(&session_id))
        .unwrap_or_else(|| PathBuf::from(&cwd));
    let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
    let checked = trusted_existing_session_cwd(&state, &filesystem, &session_id, Some(&cwd));
    let (status, message) = match checked {
        Ok(_) => ("ready", None),
        Err(error) => {
            let missing = std::fs::metadata(&original)
                .err()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            (if missing { "missing" } else { "unavailable" }, Some(error))
        }
    };
    Ok(SessionWorkspaceStatus {
        cwd: original.to_string_lossy().into_owned(),
        loaded: state.session_workspace(&session_id).is_ok(),
        status,
        message,
    })
}

#[tauri::command]
pub async fn agent_new_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    model_id: Option<String>,
    permission_mode: Option<String>,
) -> Result<String, String> {
    crate::policy::require_feature("sessions")?;
    crate::org::enforce_skill_lease();
    let cwd = {
        let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
        authorized_session_cwd(&filesystem, &cwd)?
    };
    if let Some(model_id) = model_id.as_deref() {
        crate::policy::require_model(model_id)?;
    }
    let _permission_transition_guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;
    let permission_mode =
        crate::permission_config::resolve_new_session_permission_mode(permission_mode.as_deref())?;
    require_runtime_ready(Some(&app), &state, model_id.as_deref())?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;

    // A previous attempt may have timed out while the Runtime went on to create
    // and persist the session. Adopting it keeps a retry from stacking up ghost
    // sessions the UI never learned about.
    if let Some(session_id) =
        state.take_orphaned_session(&cwd, model_id.as_deref(), &permission_mode)
    {
        tracing::info!(%session_id, "adopting session created by a timed-out request");
        if crate::policy::locked_permission_mode().is_none() {
            if let Err(error) = crate::meta::set_permission_mode(&session_id, &permission_mode) {
                // The Runtime session already exists. Put it back so a retry
                // can reclaim the same task rather than creating duplicates.
                state.record_orphaned_session(OrphanedSession {
                    session_id: session_id.clone(),
                    cwd: cwd.clone(),
                    model_id: model_id.clone(),
                    permission_mode: permission_mode.clone(),
                });
                return Err(format!(
                    "无法保存当前任务的权限模式：{error}；重试不会重复创建任务"
                ));
            }
        }
        crate::permission_config::mark_session_permission_mode_synced(
            &session_id,
            &permission_mode,
        );
        if let Err(error) = crate::meta::set_agent_mode(&session_id, "default") {
            state.record_orphaned_session(OrphanedSession {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                model_id: model_id.clone(),
                permission_mode: permission_mode.clone(),
            });
            return Err(format!(
                "无法保存当前任务的代理模式：{error}；重试不会重复创建任务"
            ));
        }
        if let Err(error) = state.record_session_workspace(&session_id, Path::new(&cwd)) {
            state.record_orphaned_session(OrphanedSession {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                model_id: model_id.clone(),
                permission_mode: permission_mode.clone(),
            });
            return Err(format!(
                "无法保存任务工作目录：{error}；重试不会重复创建任务"
            ));
        }
        crate::team_mcp::persist_registration(&tx, &session_id);
        crate::automation::persist_registration(&tx, &session_id);
        crate::office_mcp::persist_registration(&tx, &session_id);
        crate::live_info_mcp::persist_registration(&tx, &session_id);
        crate::org_mcp::reconcile_registration(&tx, &session_id);
        return Ok(session_id);
    }

    tracing::info!(
        model_id = model_id.as_deref(),
        "agent new_session command send"
    );

    let creation_key = crate::session_creation::SessionCreationKey {
        cwd: cwd.clone(),
        model_id: model_id.clone(),
        permission_mode: permission_mode.clone(),
    };
    let (mut creation, result_tx) = state.session_creations.join(&creation_key)?;
    if let Some(result_tx) = result_tx {
        let task_app = app.clone();
        let task_tx = tx.clone();
        let task_cwd = cwd.clone();
        let task_model = model_id.clone();
        let task_permission_mode = permission_mode.clone();
        let task_creation_key = creation_key.clone();
        let creation_id = creation.id;
        tokio::spawn(async move {
            let result = agent_runtime::new_session_with_options(
                &task_tx,
                &PathBuf::from(task_cwd.clone()),
                task_model.as_deref(),
                None,
                Some(&task_permission_mode),
            )
            .await
            .map_err(|error| error.to_string());
            result_tx.send_replace(Some(result.clone()));
            // Publish before taking the transition lock: an immediate retry may
            // hold it while awaiting this same request's result.
            let _permission_transition_guard =
                crate::permission_config::permission_transition_lock()
                    .lock()
                    .await;
            let task_state = task_app.state::<AppState>();
            if !task_state.is_runtime_current(&task_tx)
                || !task_state
                    .session_creations
                    .finish(&task_creation_key, creation_id)
            {
                return;
            }
            match result {
                Ok(session_id) => {
                    if !valid_session_id(&session_id) {
                        tracing::error!("Runtime returned an invalid orphaned session id");
                        return;
                    }
                    tracing::warn!(
                        %session_id,
                        "new_session completed after its caller timed out; reclaiming"
                    );
                    crate::team_mcp::persist_registration(&task_tx, &session_id);
                    crate::automation::persist_registration(&task_tx, &session_id);
                    crate::office_mcp::persist_registration(&task_tx, &session_id);
                    crate::live_info_mcp::persist_registration(&task_tx, &session_id);
                    crate::org_mcp::reconcile_registration(&task_tx, &session_id);
                    if crate::policy::locked_permission_mode().is_none() {
                        if let Err(error) =
                            crate::meta::set_permission_mode(&session_id, &task_permission_mode)
                        {
                            tracing::error!(%error, %session_id, "failed to persist orphaned task permission mode");
                        }
                    }
                    crate::permission_config::mark_session_permission_mode_synced(
                        &session_id,
                        &task_permission_mode,
                    );
                    if let Err(error) = crate::meta::set_agent_mode(&session_id, "default") {
                        tracing::error!(%error, %session_id, "failed to persist orphaned task agent mode");
                    }
                    if let Err(error) =
                        task_state.record_session_workspace(&session_id, Path::new(&task_cwd))
                    {
                        tracing::error!(%error, %session_id, "failed to persist orphaned task workspace");
                    }
                    task_state.record_orphaned_session(OrphanedSession {
                        session_id: session_id.clone(),
                        cwd: task_cwd,
                        model_id: task_model,
                        permission_mode: task_permission_mode,
                    });
                    let _ = task_app.emit(
                        "agent://session-reclaimed",
                        serde_json::json!({ "sessionId": session_id }),
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "new_session failed after its caller timed out");
                }
            }
        });
    }

    let outcome = tokio::time::timeout(NEW_SESSION_TIMEOUT, creation.wait()).await;
    if outcome.is_ok() {
        state.session_creations.finish(&creation_key, creation.id);
    }
    let session_id = match outcome {
        Err(_) => {
            tracing::error!(
                model_id = model_id.as_deref(),
                "agent new_session command timed out"
            );
            return Err("创建 Agent 会话超时（45 秒），请重试；若持续失败请重启应用。".to_string());
        }
        Ok(Err(error)) => {
            tracing::error!(
                %error,
                model_id = model_id.as_deref(),
                "agent new_session command failed"
            );
            return Err(error);
        }
        Ok(Ok(session_id)) => session_id,
    };
    if !state.is_runtime_current(&tx) {
        return Err("Agent Runtime 已重新启动，请重试创建任务".into());
    }
    if !valid_session_id(&session_id) {
        return Err("Agent Runtime 返回的会话 ID 无效".into());
    }
    tracing::info!(%session_id, "agent new_session command OK");
    if crate::policy::locked_permission_mode().is_none() {
        if let Err(error) = crate::meta::set_permission_mode(&session_id, &permission_mode) {
            // Creation succeeded remotely but the local transaction did not.
            // Keep the id reclaimable so the next attempt is idempotent.
            state.record_orphaned_session(OrphanedSession {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                model_id: model_id.clone(),
                permission_mode: permission_mode.clone(),
            });
            return Err(format!(
                "无法保存当前任务的权限模式：{error}；重试不会重复创建任务"
            ));
        }
    }
    crate::permission_config::mark_session_permission_mode_synced(&session_id, &permission_mode);
    if let Err(error) = crate::meta::set_agent_mode(&session_id, "default") {
        state.record_orphaned_session(OrphanedSession {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            model_id: model_id.clone(),
            permission_mode: permission_mode.clone(),
        });
        return Err(format!(
            "无法保存当前任务的代理模式：{error}；重试不会重复创建任务"
        ));
    }
    if let Err(error) = state.record_session_workspace(&session_id, Path::new(&cwd)) {
        state.record_orphaned_session(OrphanedSession {
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            model_id: model_id.clone(),
            permission_mode: permission_mode.clone(),
        });
        return Err(format!(
            "无法保存任务工作目录：{error}；重试不会重复创建任务"
        ));
    }
    // Team MCP remains a durable local tool. Knowledge tools are attached only
    // after the user explicitly selects sources for this task.
    crate::team_mcp::persist_registration(&tx, &session_id);
    crate::automation::persist_registration(&tx, &session_id);
    crate::office_mcp::persist_registration(&tx, &session_id);
    crate::live_info_mcp::persist_registration(&tx, &session_id);
    crate::org_mcp::reconcile_registration(&tx, &session_id);
    Ok(session_id)
}

#[tauri::command]
pub async fn agent_load_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    cwd: String,
) -> Result<Option<String>, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    crate::org::enforce_skill_lease();
    let cwd = {
        let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
        let bound = trusted_existing_session_cwd(&state, &filesystem, &session_id, Some(&cwd))?;
        crate::meta::bind_session_workspace(&session_id, &bound)?;
        bound.to_string_lossy().into_owned()
    };
    let _permission_transition_guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;
    let permission_mode = crate::permission_config::effective_session_permission_mode(&session_id);
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let current_model_id =
        agent_runtime::load_session(&tx, &session_id, &PathBuf::from(&cwd), &permission_mode)
            .await
            .map_err(|e| e.to_string())?;
    crate::permission_config::mark_session_permission_mode_synced(&session_id, &permission_mode);
    let _ = app.emit(
        "agent://permission-mode",
        crate::permission_config::permission_mode_status(true, Some(&session_id)),
    );
    state.record_session_workspace(&session_id, Path::new(&cwd))?;
    // Restore durable local MCP tools, then reapply this task's explicit
    // knowledge-source choice against the capabilities currently available.
    crate::team_mcp::persist_registration(&tx, &session_id);
    crate::automation::persist_registration(&tx, &session_id);
    crate::office_mcp::persist_registration(&tx, &session_id);
    crate::live_info_mcp::persist_registration(&tx, &session_id);
    crate::org_mcp::reconcile_registration(&tx, &session_id);
    let agent_mode = crate::meta::agent_mode(&session_id).unwrap_or_else(|| "default".into());
    if agent_mode != "default" {
        if let Err(error) =
            crate::agent_runtime::set_session_mode_id(&tx, &session_id, &agent_mode).await
        {
            tracing::warn!(%error, %session_id, %agent_mode, "failed to restore task agent mode");
        }
    }
    let automation_mode = crate::automation::AutomationMode::parse(&agent_mode)
        .unwrap_or(crate::automation::AutomationMode::Default);
    if let Err(error) = crate::automation::restore_session_mode(&session_id, automation_mode).await
    {
        tracing::warn!(%error, %session_id, %agent_mode, "failed to restore automation mode");
    }
    Ok(current_model_id)
}

#[tauri::command]
pub fn agent_list_sessions(
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
    cwd: String,
    include_archived: Option<bool>,
) -> Result<Vec<SessionSummary>, String> {
    let cwd = filesystem.require_workspace(&cwd)?;
    Ok(sessions::list_sessions(
        &cwd.to_string_lossy(),
        include_archived.unwrap_or(false),
    ))
}

/// Return the complete persisted session catalog across working directories.
/// The cwd in each row is execution context only; the frontend decides whether
/// a session belongs to a project from explicit project references.
#[tauri::command]
pub fn agent_list_all_sessions(
    include_archived: Option<bool>,
) -> Result<Vec<SessionSummary>, String> {
    sessions::list_all_sessions(include_archived.unwrap_or(false))
}

#[tauri::command]
pub async fn agent_set_knowledge_sources(
    state: State<'_, AppState>,
    session_id: String,
    personal: bool,
    organization: bool,
    organization_scope_ids: Option<Vec<String>>,
) -> Result<KnowledgeSourcesView, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let organization_scope_ids = organization_scope_ids.unwrap_or_default();
    if organization_scope_ids.len() > 64
        || organization_scope_ids.iter().any(|scope_id| {
            scope_id.trim().is_empty()
                || scope_id.chars().count() > 256
                || scope_id
                    .chars()
                    .any(|character| character.is_control() || character == ',')
        })
    {
        return Err("组织知识范围无效或过多".into());
    }
    crate::org_mcp::update_session_configuration(
        &tx,
        &session_id,
        personal,
        organization,
        organization_scope_ids,
    )
    .await?;
    let active = crate::org_mcp::effective_selection(crate::org_mcp::KnowledgeSourceSelection {
        personal,
        organization,
    });
    Ok(KnowledgeSourcesView {
        personal_selected: personal,
        organization_selected: organization,
        personal_attached: active.personal,
        organization_attached: active.organization,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSourcesView {
    personal_selected: bool,
    organization_selected: bool,
    personal_attached: bool,
    organization_attached: bool,
}

#[tauri::command]
// Keep these arguments flat to preserve the existing Tauri IPC payload contract.
#[allow(clippy::too_many_arguments)]
pub async fn agent_send(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    text: String,
    attachments: Option<Vec<String>>,
    display_text: Option<String>,
    prompt_id: Option<String>,
    send_now: Option<bool>,
) -> Result<(), String> {
    validate_send_payload(&session_id, &text, display_text.as_deref())?;
    if prompt_id
        .as_deref()
        .is_some_and(|value| !valid_session_id(value))
    {
        return Err("消息 ID 无效或过长".into());
    }
    require_runtime_ready(Some(&app), &state, None)?;
    state.session_workspace(&session_id)?;
    let workspace = trusted_existing_session_cwd(
        &state,
        &app.state::<crate::shell_fs::FilesystemAccess>(),
        &session_id,
        None,
    )?;
    let attachments = attachments.unwrap_or_default();
    let attachments = if attachments.is_empty() {
        Vec::new()
    } else {
        app.state::<crate::shell_fs::FilesystemAccess>()
            .validate_session_attachments(&workspace, &attachments)?
    };
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let send_now = send_now.unwrap_or(false);
    let task_admitted = crate::automation::prepare_user_prompt(
        &tx,
        &session_id,
        display_text.as_deref().unwrap_or(&text),
        !attachments.is_empty(),
        send_now,
    )
    .await?;
    tracing::info!(%session_id, attachment_count = attachments.len(), send_now, "agent prompt send");
    let result = agent_runtime::prompt_with_attachments(
        &tx,
        &session_id,
        &text,
        &attachments,
        display_text.as_deref(),
        prompt_id.as_deref(),
        send_now,
    )
    .await
    .map_err(|error| {
        tracing::error!(%session_id, error = ?error, "agent prompt failed");
        error.to_string()
    });
    if let (true, Some(generation)) = (result.is_err(), task_admitted) {
        crate::automation::prompt_admission_failed(&session_id, generation).await;
    }
    result
}

#[tauri::command]
pub async fn agent_cancel(
    state: State<'_, AppState>,
    session_id: String,
    cancel_action: Option<String>,
    prompt_id: Option<String>,
) -> Result<(), String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let cancel_trigger = match cancel_action.as_deref().unwrap_or("stop") {
        "pause" => "pause",
        "stop" => "stop",
        _ => return Err("无效的取消动作".into()),
    };
    let automation_result = if cancel_trigger == "pause" {
        crate::automation::pause_session(&session_id).await
    } else {
        crate::automation::stop_session(&session_id).await
    };
    if let Err(ref error) = automation_result {
        tracing::warn!(%error, %session_id, cancel_trigger, "failed to stop automation while cancelling turn");
    }
    agent_runtime::cancel(&tx, &session_id, cancel_trigger, prompt_id.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    automation_result
}

/// Cleanly shut down the agent (cancel token + clear state) so the frontend
/// can call `agent_init` again to restart. Used after `agent://agent-died`.
#[tauri::command]
pub async fn agent_shutdown(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    permissions: State<'_, Permissions>,
    questions: State<'_, Questions>,
    plan_approvals: State<'_, PlanApprovals>,
    folder_trusts: State<'_, FolderTrusts>,
) -> Result<(), String> {
    // Trigger the cancel token so the agent thread's `cancelled().await` resolves.
    let _init_guard = state.init_lock.lock().await;
    crate::automation::shutdown_all().await?;
    stop_agent_runtime_wait(&state, RUNTIME_SHUTDOWN_GRACE).await?;
    if let Some(scheduler) = state.automation_scheduler.lock().unwrap().take() {
        scheduler.abort();
    }
    crate::automations::clear_runtime_sessions();
    crate::org_mcp::clear_session_selections().await;
    state.clear_orphaned_sessions();
    state.clear_session_workspaces();
    crate::agent_admin::clear_runtime_capabilities();
    state.init_outcome.lock().unwrap().take();
    let (closed_permissions, _, _, _) = tokio::join!(
        permissions.cancel_all(),
        questions.cancel_all(),
        plan_approvals.cancel_all(),
        folder_trusts.cancel_all(),
    );
    for notice in closed_permissions {
        crate::bridge::emit_permission_closed(&app, notice);
    }
    state.mark_runtime_models_initializing();
    tracing::info!("EchoAgent agent shut down (ready for re-init)");
    Ok(())
}

// Shared across exit, restart and re-init. Network memory work is checkpointed.
const RUNTIME_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Stop the embedded Runtime without dropping session-end persistence hooks.
/// Shared by the frontend restart command and the native tray Quit action.
pub(crate) async fn stop_agent_runtime(state: &AppState) -> Result<(), String> {
    stop_agent_runtime_with_grace(state, RUNTIME_SHUTDOWN_GRACE).await
}

async fn stop_agent_runtime_with_grace(
    state: &AppState,
    grace: std::time::Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + grace;
    let _init_guard = tokio::time::timeout_at(deadline, state.init_lock.lock())
        .await
        .map_err(|_| "AI 引擎仍在初始化，应用已保留，请稍后重试退出".to_string())?;
    stop_agent_runtime_wait(
        state,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
    ).await
}

async fn stop_agent_runtime_wait(
    state: &AppState,
    grace: std::time::Duration,
) -> Result<(), String> {
    // The scheduler owns a sender independently of state.tx. Quiesce it
    // before flushing, otherwise a due task can create another session while
    // the old Runtime is being saved for exit/restart.
    if let Some(scheduler) = state.automation_scheduler.lock().unwrap().take() {
        scheduler.abort();
    }
    let handle = state.handle.lock().unwrap().take();
    state.tx.lock().unwrap().take();
    if let Some(mut handle) = handle {
        handle.cancel.cancel();
        let result = tokio::time::timeout(grace, async {
            if *handle.shutdown_complete.borrow() {
                return Ok(());
            }
            handle
                .shutdown_complete
                .wait_for(|done| *done)
                .await
                .map(|_| ())
                .map_err(|_| {
                    "AI 引擎已退出，但未能确认任务数据保存完成，请检查会话后重试".to_string()
                })
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                state.mark_runtime_models_failed(error.clone());
                crate::permission_config::mark_runtime_permission_mode_offline(None);
                return Err(error);
            }
            Err(_) => {
                // Cancellation stops new work but must not drop a LocalSet
                // whose session-end persistence hooks are still flushing.
                let failure = handle
                    .shutdown_failure
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                *state.handle.lock().unwrap() = Some(handle);
                let error = match failure {
                    Some(reason) => format!("任务数据保存尚未完成：{reason}。已保留应用与后台保存任务，请修复后重试退出或初始化"),
                    None => "任务数据仍在保存，已保留应用与后台保存任务，请稍后重试退出或初始化".to_string(),
                };
                state.mark_runtime_models_failed(error.clone());
                crate::permission_config::mark_runtime_permission_mode_offline(None);
                return Err(error);
            }
        }
    }
    crate::permission_config::mark_runtime_permission_mode_offline(None);
    Ok(())
}

/// Resolve a pending permission request from the frontend.
#[tauri::command]
pub async fn agent_resolve_permission(
    app: tauri::AppHandle,
    permissions: State<'_, Permissions>,
    request_id: String,
    option_id: Option<String>,
    cancelled: Option<bool>,
) -> Result<bool, String> {
    let outcome = match (cancelled.unwrap_or(false), option_id) {
        (true, _) => PermissionOutcome::Cancelled,
        (false, Some(id)) => PermissionOutcome::Selected(id),
        (false, None) => PermissionOutcome::Cancelled,
    };
    let notice = permissions.resolve(&request_id, outcome).await;
    if let Some(notice) = notice.as_ref().cloned() {
        crate::bridge::emit_permission_closed(&app, notice);
    }
    Ok(notice.is_some())
}

/// Return all currently parked agent→client interactions. The frontend calls
/// this after wiring event listeners, making permission/question/plan/trust
/// requests replayable across renderer reloads and background-session switches.
#[tauri::command]
pub async fn agent_list_pending_interactions(
    permissions: State<'_, Permissions>,
    questions: State<'_, Questions>,
    plan_approvals: State<'_, PlanApprovals>,
    folder_trusts: State<'_, FolderTrusts>,
    session_id: Option<String>,
) -> Result<PendingInteractionsFrontend, String> {
    Ok(crate::bridge::pending_interactions(
        &permissions,
        &questions,
        &plan_approvals,
        &folder_trusts,
        session_id.as_deref(),
    )
    .await)
}

/// Fulfil the exact `echo.agent/exit_plan_mode` reverse request. Unknown
/// outcomes are rejected instead of being interpreted as approval.
#[tauri::command]
pub async fn agent_resolve_plan_approval(
    plan_approvals: State<'_, PlanApprovals>,
    request_id: String,
    outcome: String,
    feedback: Option<String>,
) -> Result<bool, String> {
    let outcome = match outcome.as_str() {
        "approved" => PlanApprovalOutcome::Approved,
        "cancelled" => PlanApprovalOutcome::Cancelled {
            feedback: feedback.filter(|value| !value.trim().is_empty()),
        },
        "abandoned" => PlanApprovalOutcome::Abandoned,
        other => return Err(format!("invalid plan approval outcome: {other}")),
    };
    Ok(plan_approvals.resolve(&request_id, outcome).await)
}

/// Resolve a pending question request from the frontend.
///
/// Wire contract for EchoAgent's `AskUserQuestionExtResponse`:
/// - `outcome: "cancelled"` → `{ "outcome": "cancelled" }`
/// - `outcome: "accepted"` → `{ "outcome": "accepted", ... }`
/// - plan-only actions preserve partial answers as `chat_about_this` or
///   `skip_interview`.
///
/// `answers` must be keyed by **question text** (not synthetic id). Values may
/// be a string or a list of strings (multi-select). Freeform answers use
/// label `"Other"` with the typed text in `annotations[question].notes`.
#[tauri::command]
pub async fn agent_resolve_question(
    questions: State<'_, Questions>,
    request_id: String,
    mut answers: Option<std::collections::HashMap<String, serde_json::Value>>,
    annotations: Option<std::collections::HashMap<String, QuestionAnnotationDto>>,
    partial_answers: Option<std::collections::HashMap<String, String>>,
    outcome: Option<String>,
    cancelled: Option<bool>,
) -> Result<bool, String> {
    validate_question_payload(
        &request_id,
        answers.as_ref(),
        annotations.as_ref(),
        partial_answers.as_ref(),
        outcome.as_deref(),
    )?;
    let requested = if cancelled.unwrap_or(false) {
        "cancelled"
    } else {
        outcome.as_deref().unwrap_or(if answers.is_some() {
            "accepted"
        } else {
            "cancelled"
        })
    };
    let outcome = match requested {
        "cancelled" => QuestionOutcome::Cancelled,
        "chat_about_this" => QuestionOutcome::ChatAboutThis {
            partial_answers: partial_answers.unwrap_or_default(),
        },
        "skip_interview" => QuestionOutcome::SkipInterview {
            partial_answers: partial_answers.unwrap_or_default(),
        },
        "accepted" => {
            let mut normalized = std::collections::HashMap::new();
            for (k, v) in answers.take().unwrap_or_default() {
                let labels = match v {
                    serde_json::Value::String(s) => {
                        if s.is_empty() {
                            continue;
                        }
                        vec![s]
                    }
                    serde_json::Value::Array(arr) => arr
                        .into_iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .filter(|s| !s.is_empty())
                        .collect(),
                    _ => continue,
                };
                if !labels.is_empty() {
                    normalized.insert(k, labels);
                }
            }
            let anns = annotations.map(|m| {
                m.into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            crate::bridge::QuestionAnnotation {
                                preview: v.preview,
                                notes: v.notes,
                            },
                        )
                    })
                    .collect()
            });
            QuestionOutcome::Accepted {
                answers: normalized,
                annotations: anns,
            }
        }
        other => return Err(format!("invalid question outcome: {other}")),
    };
    Ok(questions.resolve(&request_id, outcome).await)
}

/// DTO for per-question annotations from the frontend.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnnotationDto {
    pub preview: Option<String>,
    pub notes: Option<String>,
}

fn validate_question_payload(
    request_id: &str,
    answers: Option<&std::collections::HashMap<String, serde_json::Value>>,
    annotations: Option<&std::collections::HashMap<String, QuestionAnnotationDto>>,
    partial_answers: Option<&std::collections::HashMap<String, String>>,
    outcome: Option<&str>,
) -> Result<(), String> {
    if !valid_session_id(request_id) {
        return Err("问题请求 ID 无效或过长".into());
    }
    if outcome.is_some_and(|value| value.len() > 32 || value.chars().any(char::is_control)) {
        return Err("问题响应类型无效".into());
    }
    let answers_len = answers.map_or(0, HashMap::len);
    let annotations_len = annotations.map_or(0, HashMap::len);
    let partial_len = partial_answers.map_or(0, HashMap::len);
    if answers_len > MAX_QUESTION_FIELDS
        || annotations_len > MAX_QUESTION_FIELDS
        || partial_len > MAX_QUESTION_FIELDS
    {
        return Err(format!("问题响应字段不能超过 {MAX_QUESTION_FIELDS} 个"));
    }
    let valid_key = |value: &str| {
        !value.trim().is_empty()
            && value.chars().count() <= MAX_QUESTION_KEY_CHARS
            && !value.chars().any(|character| character == '\0')
    };
    let valid_value = |value: &str| {
        value.chars().count() <= MAX_QUESTION_VALUE_CHARS
            && !value.chars().any(|character| character == '\0')
    };
    if answers.is_some_and(|items| {
        items.iter().any(|(key, value)| {
            !valid_key(key)
                || match value {
                    serde_json::Value::String(value) => !valid_value(value),
                    serde_json::Value::Array(values) => {
                        values.len() > MAX_QUESTION_SELECTIONS
                            || values
                                .iter()
                                .any(|value| value.as_str().is_none_or(|value| !valid_value(value)))
                    }
                    _ => true,
                }
        })
    }) || annotations.is_some_and(|items| {
        items.iter().any(|(key, value)| {
            !valid_key(key)
                || value
                    .preview
                    .as_deref()
                    .is_some_and(|value| !valid_value(value))
                || value
                    .notes
                    .as_deref()
                    .is_some_and(|value| !valid_value(value))
        })
    }) || partial_answers.is_some_and(|items| {
        items
            .iter()
            .any(|(key, value)| !valid_key(key) || !valid_value(value))
    }) {
        return Err("问题响应包含无效或过长字段".into());
    }
    let encoded = serde_json::to_vec(&(answers, annotations, partial_answers))
        .map_err(|error| format!("无法校验问题响应：{error}"))?;
    if encoded.len() > MAX_QUESTION_RESPONSE_BYTES {
        return Err("问题响应不能超过 4 MiB".into());
    }
    Ok(())
}

/// Switch the model used by an existing session. Maps to EchoAgent's
/// `session/set_model`. May reject with `MODEL_SWITCH_INCOMPATIBLE_AGENT`
/// if the session has turns and the new model needs a different harness —
/// the error string is forwarded verbatim so the UI can prompt accordingly.
#[tauri::command]
pub async fn agent_set_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    model_id: String,
) -> Result<(), String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    crate::policy::require_model(&model_id)?;
    require_runtime_ready(Some(&app), &state, Some(&model_id))?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    agent_runtime::set_session_model(&tx, &session_id, &model_id)
        .await
        .map_err(|e| e.to_string())
}

/// List every working directory EchoAgent has seen (deduplicated), with a session
/// count per cwd. Used to populate the Composer's workspace picker.
#[tauri::command]
pub fn agent_list_workspaces(
    filesystem: State<'_, crate::shell_fs::FilesystemAccess>,
) -> Vec<WorkspaceInfo> {
    let workspaces = sessions::list_workspaces();
    for workspace in &workspaces {
        if let Err(error) = filesystem.authorize_workspace(&workspace.cwd) {
            tracing::warn!(%error, cwd = %workspace.cwd, "persisted workspace was not added to filesystem allow-list");
        }
    }
    workspaces
}

/// Rename a session via EchoAgent's `echo.agent/session/rename` extension method. On
/// success EchoAgent also broadcasts `SessionSummaryGenerated`, which our bridge
/// forwards as the `agent://summary` event — so the frontend will receive the
/// new title twice (once from this return, once from the event). That's fine:
/// both arrive at the same store `upsert` and are idempotent.
#[tauri::command]
pub async fn agent_rename_session(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
    cwd: Option<String>,
) -> Result<(), String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if title.trim().is_empty()
        || title.chars().count() > 512
        || title.chars().any(|character| character == '\0')
    {
        return Err("会话标题为空、过长或包含非法字符".into());
    }
    let workspace = trusted_session_history_cwd(&state, &session_id, cwd.as_deref())?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let workspace = workspace.to_string_lossy().into_owned();
    agent_runtime::rename_session(&tx, &session_id, &title, Some(&workspace))
        .await
        .map_err(|e| e.to_string())
}

/// Delete a session's persisted history via EchoAgent's `echo.agent/session/delete`.
/// The generated memory summaries and their search-index chunks are removed
/// as a cascade so deleting a chat has one predictable privacy boundary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteResult {
    pub memory_summaries_deleted: usize,
    pub memory_cleanup_warning: Option<String>,
}

#[tauri::command]
pub async fn agent_delete_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    cwd: Option<String>,
) -> Result<SessionDeleteResult, String> {
    delete_session_data(&app, &state, &session_id, cwd.as_deref()).await
}

pub(crate) async fn delete_session_data(
    _app: &tauri::AppHandle,
    state: &AppState,
    session_id: &str,
    cwd: Option<&str>,
) -> Result<SessionDeleteResult, String> {
    if !valid_session_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    let workspace = trusted_session_history_cwd(state, session_id, cwd)?;
    delete_owned_session_data(state, session_id, &workspace).await
}

/// The caller proves ownership before entering this helper. Group cleanup uses
/// its durable session ledger and native catalog, so history remains deletable
/// even after the original working directory was moved or removed.
pub(crate) async fn delete_owned_session_data(
    state: &AppState,
    session_id: &str,
    workspace: &std::path::Path,
) -> Result<SessionDeleteResult, String> {
    if !valid_session_id(session_id) || !workspace.is_absolute() {
        return Err("会话标识或原工作区路径无效".into());
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let workspace_arg = workspace.to_string_lossy().into_owned();
    // Stop side effects before history deletion. A failed stop remains
    // retryable and must not leave an active browser belonging to a removed task.
    crate::automation::stop_existing_session(session_id).await?;
    agent_runtime::delete_session(&tx, session_id, Some(&workspace_arg))
        .await
        .map_err(|e| e.to_string())?;
    Ok(forget_deleted_session(state, workspace, session_id).await)
}

/// Group ownership is proven by the durable member ledger. Locate by exact ID
/// across historical workspaces, including records whose summary is damaged.
pub(crate) async fn delete_group_session_data(
    state: &AppState,
    session_id: &str,
    workspace: &std::path::Path,
) -> Result<SessionDeleteResult, String> {
    if !valid_session_id(session_id) || !workspace.is_absolute() {
        return Err("会话标识或原工作区路径无效".into());
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("Agent Runtime 尚未就绪，请稍后重试")?;
    crate::automation::stop_existing_session(session_id).await?;
    agent_runtime::delete_session(&tx, session_id, None)
        .await
        .map_err(|e| e.to_string())?;
    if !echo_agent_runtime::session::persistence::session_history_paths(session_id, None)
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Err("内部执行历史尚未清理完成".into());
    }
    Ok(forget_deleted_session(state, workspace, session_id).await)
}

pub(crate) async fn forget_deleted_session(
    state: &AppState,
    workspace: &std::path::Path,
    session_id: &str,
) -> SessionDeleteResult {
    let (memory_summaries_deleted, mut memory_cleanup_warning) =
        match crate::agent_admin::delete_session_memory_artifacts(workspace, session_id) {
            Ok(count) => (count, None),
            Err(error) => {
                tracing::warn!(
                    %error,
                    %session_id,
                    "session deleted but generated memory cleanup failed"
                );
                (0, Some(error))
            }
        };
    state.forget_session_workspace(session_id);
    if let Err(error) = crate::team_mcp::forget_session(session_id) {
        tracing::warn!(%error, %session_id, "session deleted but team registry cleanup failed");
        memory_cleanup_warning = Some(match memory_cleanup_warning {
            Some(previous) => {
                format!("{previous}；会话团队记录清理失败，下次启动时会重试：{error}")
            }
            None => format!("会话团队记录清理失败，下次启动时会重试：{error}"),
        });
    }
    crate::automation::forget_session(session_id).await;
    crate::org_mcp::forget_session_selection(session_id).await;
    crate::permission_config::forget_session_permission_mode(session_id);
    if let Err(error) = crate::knowledge_traces::forget_session(session_id) {
        memory_cleanup_warning = Some(format!(
            "{}知识来源记录清理失败：{error}",
            memory_cleanup_warning
                .map(|e| format!("{e}；"))
                .unwrap_or_default()
        ));
    }
    if let Err(error) = crate::session_artifacts::forget_session(session_id) {
        memory_cleanup_warning = Some(format!(
            "{}会话交付物记录清理失败：{error}",
            memory_cleanup_warning
                .map(|e| format!("{e}；"))
                .unwrap_or_default()
        ));
    }
    if let Err(error) = crate::meta::clear_session_metadata(session_id) {
        tracing::warn!(%error, %session_id, "session deleted but sidecar metadata cleanup failed");
        memory_cleanup_warning = Some(match memory_cleanup_warning {
            Some(previous) => format!("{previous}；会话附属信息清理失败：{error}"),
            None => format!("会话附属信息清理失败：{error}"),
        });
    }
    SessionDeleteResult {
        memory_summaries_deleted,
        memory_cleanup_warning,
    }
}

/// Pin or unpin a session. EchoAgent's `Summary` has no `pinned` field, so this is
/// EchoAgent-only state stored in `~/.echo-agent/echoagent-state.json`. Returns the
/// new pinned value so the frontend can update without a re-fetch.
#[tauri::command]
pub fn agent_set_session_pinned(
    state: State<'_, AppState>,
    session_id: String,
    pinned: bool,
) -> Result<bool, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if state.session_workspace(&session_id).is_err()
        && !sessions::persisted_session_exists(&session_id)?
    {
        return Err("会话不存在或已被删除，无法修改置顶状态".into());
    }
    crate::meta::set_pinned(&session_id, pinned)
}

/// Fetch the session's context-window snapshot (`echo.agent/session/info`) for the
/// composer's context-usage pill/popover. Fails when the session isn't live
/// in the agent (e.g. an old session never loaded this launch) — the
/// frontend treats that as "no data" and hides the pill.
#[tauri::command]
pub async fn agent_session_info(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    agent_runtime::session_info(&tx, &session_id)
        .await
        .map_err(|e| e.to_string())
}

/// Fetch the session's cumulative token usage (`echo.agent/session/usage`) — used
/// by the context-usage popover for the average cache hit rate.
#[tauri::command]
pub async fn agent_session_usage(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    agent_runtime::session_usage(&tx, &session_id)
        .await
        .map_err(|e| e.to_string())
}

/// Archive or unarchive a session. EchoAgent's `Summary` has no `archived` field,
/// so this is EchoAgent-only state stored in `~/.echo-agent/echoagent-state.json`.
/// Archived sessions are filtered by default from `list_sessions`. Returns the
/// new archived value so the frontend can update without a re-fetch.
#[tauri::command]
pub fn agent_set_session_archived(
    state: State<'_, AppState>,
    session_id: String,
    archived: bool,
) -> Result<bool, String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    if state.session_workspace(&session_id).is_err()
        && !sessions::persisted_session_exists(&session_id)?
    {
        return Err("会话不存在或已被删除，无法修改归档状态".into());
    }
    crate::meta::set_archived(&session_id, archived)
}

/// Persist the latest turn lifecycle state for task lists and project activity.
/// Only a backend-known session may write metadata; observation timestamps are
/// validated and future-bounded before they can affect catalog ordering.
#[tauri::command]
pub fn agent_set_session_status(
    state: State<'_, AppState>,
    session_id: String,
    status: String,
    updated_at: String,
) -> Result<(), String> {
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(&session_id)?;
    let observed_at = chrono::DateTime::parse_from_rfc3339(&updated_at)
        .map_err(|_| "会话状态时间无效".to_string())?
        .with_timezone(&chrono::Utc);
    if observed_at > chrono::Utc::now() + chrono::Duration::minutes(5) {
        return Err("会话状态时间超出允许范围".into());
    }
    crate::meta::set_session_status(&session_id, &status, &updated_at)
}

/// Bind an expert to a session (EchoAgent-only state). Returns `true` on success.
#[tauri::command]
// Flat arguments preserve the existing Tauri IPC contract.
#[allow(clippy::too_many_arguments)]
pub fn agent_set_session_expert(
    state: State<'_, AppState>,
    session_id: String,
    expert_id: String,
    expert_name: String,
    source: String,
    avatar_local: Option<String>,
    expert_path: Option<String>,
    expected_raw: Option<String>,
) -> Result<bool, String> {
    // Requiring a live backend binding prevents a forged renderer payload from
    // attaching metadata to arbitrary historical session ids. Expert fields,
    // especially local avatar paths, are resolved against exact rows emitted
    // by the local agent store rather than trusted from IPC.
    if !valid_session_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    let cwd = state.session_workspace(&session_id)?;
    crate::agents_store::require_expert_snapshot(
        &cwd,
        &expert_name,
        expert_path.as_deref().ok_or("请选择有效的本地专家")?,
        expected_raw
            .as_deref()
            .ok_or("专家定义版本缺失，请重新选择")?,
    )?;
    let binding = match source.as_str() {
        "local" => crate::agents_store::require_listed_local_expert(
            &expert_id,
            &expert_name,
            avatar_local.as_deref(),
        )?,
        _ => return Err("专家来源未经后端授权".into()),
    };
    crate::meta::set_expert(&session_id, binding)
}

/// Remove the expert binding from a session. Returns `true` if a binding was removed.
#[tauri::command]
pub fn agent_clear_session_expert(session_id: String) -> Result<bool, String> {
    crate::meta::clear_expert(&session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shutdown_fixture(state: &AppState) -> tokio::sync::watch::Sender<bool> {
        let (client, _agent) = echo_agent_acp::acp_channels();
        let (finished, shutdown_complete) = tokio::sync::watch::channel(false);
        *state.tx.lock().unwrap() = Some(client.tx.clone());
        *state.handle.lock().unwrap() = Some(AgentHandle {
            tx: client.tx,
            rx: client.rx,
            cancel: tokio_util::sync::CancellationToken::new(),
            shutdown_complete,
            shutdown_failure: std::sync::Arc::new(Mutex::new(None)),
            gateway_failure: tokio::sync::watch::channel(None).1,
            thread: None,
        });
        finished
    }

    #[tokio::test]
    async fn shutdown_timeout_retains_the_runtime_until_a_save_retry_completes() {
        let state = AppState::default();
        *state.automation_scheduler.lock().unwrap() =
            Some(tauri::async_runtime::spawn(std::future::pending()));
        let finished = shutdown_fixture(&state);
        let error = stop_agent_runtime_wait(&state, std::time::Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(error.contains("仍在保存"));
        assert!(
            state.automation_scheduler.lock().unwrap().is_none(),
            "scheduler must stop before persistence, including a retryable timeout"
        );
        assert!(
            state.tx.lock().unwrap().is_none(),
            "cancelled runtime must not accept new prompts"
        );
        assert!(state
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel
            .is_cancelled());
        finished.send(true).unwrap();
        stop_agent_runtime_wait(&state, std::time::Duration::from_millis(5))
            .await
            .unwrap();
        assert!(state.handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn closed_shutdown_channel_is_not_a_successful_save_ack() {
        let state = AppState::default();
        drop(shutdown_fixture(&state));
        let error = stop_agent_runtime_wait(&state, std::time::Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(error.contains("未能确认任务数据保存完成"));
        assert!(
            state.handle.lock().unwrap().is_none(),
            "an exited thread cannot be retried as a live worker"
        );
    }

    #[tokio::test]
    async fn shutdown_timeout_surfaces_the_actual_persistence_failure() {
        let state = AppState::default();
        let _finished = shutdown_fixture(&state);
        *state
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .shutdown_failure
            .lock()
            .unwrap() = Some("磁盘写入失败：permission denied".into());
        let error = stop_agent_runtime_wait(&state, std::time::Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(error.contains("permission denied"));
        assert!(state.handle.lock().unwrap().is_some());
    }

    #[test]
    fn failed_gateway_is_not_reused_while_its_worker_is_still_saving() {
        let state = AppState::default();
        let _finished = shutdown_fixture(&state);
        let old_tx = state.tx.lock().unwrap().clone().unwrap();
        assert!(state.quarantine_gateway_if_current(
            &old_tx,
            "gateway closed while disk save is pending".into()
        ));
        assert!(state.tx.lock().unwrap().is_none());
        assert!(
            state.handle.lock().unwrap().is_some(),
            "the saving worker must remain retryable"
        );
        let (replacement, _agent) = echo_agent_acp::acp_channels();
        *state.tx.lock().unwrap() = Some(replacement.tx.clone());
        assert!(!state.quarantine_gateway_if_current(&old_tx, "late old failure".into()));
        assert!(state
            .tx
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .same_channel(&replacement.tx));
    }

    #[test]
    fn gateway_quarantine_keeps_sender_identity_until_readiness_is_invalidated() {
        let state = std::sync::Arc::new(AppState::default());
        let _finished = shutdown_fixture(&state);
        let old_tx = state.tx.lock().unwrap().clone().unwrap();
        let cancel = state
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel
            .clone();
        let readiness_guard = state.runtime_models.lock().unwrap();
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            worker_state.quarantine_gateway_if_current(&old_tx, "gateway failed".into())
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let reached_readiness = cancel.is_cancelled();
        let replacement_blocked = matches!(
            state.tx.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );
        drop(readiness_guard);
        assert!(worker.join().unwrap());
        assert!(
            reached_readiness,
            "quarantine must reach the blocked readiness transition"
        );
        assert!(
            replacement_blocked,
            "replacement cannot publish its sender in the middle of old-runtime quarantine"
        );
    }

    #[tokio::test]
    async fn shutdown_waits_for_an_in_progress_initialization() {
        let state = AppState::default();
        let init_guard = state.init_lock.lock().await;
        let mut stop = Box::pin(stop_agent_runtime(&state));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), &mut stop)
                .await
                .is_err()
        );
        let finished = shutdown_fixture(&state);
        finished.send(true).unwrap();
        drop(init_guard);
        stop.await.unwrap();
        assert!(state.handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn shutdown_initialization_wait_is_bounded_without_discarding_the_runtime() {
        let state = AppState::default();
        let _init_guard = state.init_lock.lock().await;
        let _finished = shutdown_fixture(&state);
        let error = stop_agent_runtime_with_grace(&state, std::time::Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(error.contains("仍在初始化"));
        assert!(state.handle.lock().unwrap().is_some());
        assert!(state.tx.lock().unwrap().is_some());
        assert!(!state
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel
            .is_cancelled());
    }

    #[test]
    fn renderer_message_and_question_payloads_are_bounded() {
        assert!(validate_send_payload("session", "hello", Some("hello")).is_ok());
        assert!(validate_send_payload("session", &"x".repeat(MAX_PROMPT_BYTES + 1), None).is_err());
        assert!(
            validate_send_payload(&"s".repeat(MAX_SESSION_ID_CHARS + 1), "hello", None).is_err()
        );

        let valid_answers =
            HashMap::from([("Which?".into(), serde_json::json!(["first", "second"]))]);
        assert!(validate_question_payload(
            "request",
            Some(&valid_answers),
            None,
            None,
            Some("accepted")
        )
        .is_ok());
        let oversized = HashMap::from([(
            "Which?".into(),
            serde_json::Value::String("x".repeat(MAX_QUESTION_VALUE_CHARS + 1)),
        )]);
        assert!(validate_question_payload(
            "request",
            Some(&oversized),
            None,
            None,
            Some("accepted")
        )
        .is_err());
    }

    fn runtime_state(revision: &str, model_ids: &[&str]) -> RuntimeModelState {
        let (client, _agent) = echo_agent_acp::acp_channels();
        RuntimeModelState {
            initialized: true,
            revision: Some(revision.into()),
            model_ids: model_ids.iter().map(|id| (*id).into()).collect(),
            auth_method_id: Some("echoagent.api_key".into()),
            last_error: None,
            sender: Some(client.tx),
        }
    }

    #[test]
    fn automatic_model_resolution_uses_only_configured_live_models() {
        let configured = vec!["model-a".to_string(), "model-b".to_string()];
        let runtime = vec![
            "bundled-default".to_string(),
            "model-a".to_string(),
            "model-b".to_string(),
        ];

        assert_eq!(
            effective_configured_model_id(&configured, &runtime, Some("model-b")).as_deref(),
            Some("model-b")
        );
        assert_eq!(
            effective_configured_model_id(&configured, &runtime, Some("bundled-default"))
                .as_deref(),
            Some("model-a")
        );
        assert_eq!(
            effective_configured_model_id(&configured, &["bundled-default".into()], None),
            None
        );
    }

    #[test]
    fn automatic_model_resolution_prefers_explicit_then_organization_then_personal_then_builtin() {
        let configured = vec![
            "echoagent-ojlab/chat-glm".into(),
            "echoagent-ojlab/chat-xc".into(),
            "my-model".into(),
            "organization/team-model".into(),
        ];
        assert_eq!(
            effective_configured_model_id(&configured, &configured, None).as_deref(),
            Some("organization/team-model")
        );
        assert_eq!(
            effective_configured_model_id(&configured, &configured, Some("my-model")).as_deref(),
            Some("my-model")
        );
        assert_eq!(
            effective_configured_model_id(&configured, &configured[..3], None).as_deref(),
            Some("my-model")
        );
        assert_eq!(
            effective_configured_model_id(&configured, &configured[..2], None).as_deref(),
            Some("echoagent-ojlab/chat-xc")
        );
    }

    #[test]
    fn runtime_readiness_requires_current_matching_snapshot() {
        let disk_models = vec!["model-a".to_string()];

        let unacknowledged = auth_status_from_snapshots(
            disk_models.clone(),
            None,
            "rev-a",
            RuntimeModelState::default(),
            false,
        );
        assert!(!unacknowledged.ready);
        assert!(!unacknowledged.runtime_ready);

        let stale_revision = auth_status_from_snapshots(
            disk_models.clone(),
            None,
            "rev-b",
            runtime_state("rev-a", &["model-a"]),
            true,
        );
        assert!(!stale_revision.ready);
        assert!(!stale_revision.synchronized);

        let retired_runtime = auth_status_from_snapshots(
            disk_models.clone(),
            None,
            "rev-a",
            runtime_state("rev-a", &["model-a"]),
            false,
        );
        assert!(!retired_runtime.ready);

        let synchronized = auth_status_from_snapshots(
            disk_models,
            None,
            "rev-a",
            runtime_state("rev-a", &["model-a", "runtime-default"]),
            true,
        );
        assert!(synchronized.ready);
        assert!(synchronized.synchronized);
        assert!(synchronized.runtime_auth_ready);
        assert_eq!(
            validate_runtime_ready(&synchronized, Some("model-a")),
            Ok(())
        );
    }

    #[test]
    fn runtime_catalog_without_auth_acknowledgement_is_not_ready() {
        let mut runtime = runtime_state("rev-a", &["model-a"]);
        runtime.auth_method_id = None;
        let status =
            auth_status_from_snapshots(vec!["model-a".into()], None, "rev-a", runtime, true);

        assert!(status.synchronized, "the catalog revision did match");
        assert!(!status.runtime_auth_ready);
        assert!(!status.runtime_ready);
        assert!(!status.ready);
        assert!(status.reason.as_deref().unwrap().contains("凭证同步"));
        assert!(validate_runtime_ready(&status, Some("model-a")).is_err());
    }

    #[test]
    fn runtime_error_and_unavailable_requested_model_block_commands() {
        let mut failed = runtime_state("rev-a", &["model-a"]);
        failed.initialized = false;
        failed.last_error = Some("reload rejected".into());
        let failed_status =
            auth_status_from_snapshots(vec!["model-a".into()], None, "rev-a", failed, true);
        assert!(!failed_status.ready);
        assert!(failed_status
            .reason
            .as_deref()
            .unwrap()
            .contains("reload rejected"));
        assert!(validate_runtime_ready(&failed_status, None).is_err());

        let ready_status = auth_status_from_snapshots(
            vec!["model-a".into()],
            None,
            "rev-a",
            runtime_state("rev-a", &["model-a"]),
            true,
        );
        let error = validate_runtime_ready(&ready_status, Some("model-b")).unwrap_err();
        assert!(error.contains("model-b"));
    }

    /// A model that `[models]` hides or disables never reaches the Runtime's
    /// ACP catalog. Readiness must follow the Runtime's effective catalog, not a
    /// disk ⊆ runtime subset test, or one hidden model locks chat permanently.
    #[test]
    fn hidden_or_disabled_disk_model_does_not_block_readiness() {
        let status = auth_status_from_snapshots(
            vec!["model-a".into(), "model-hidden".into()],
            None,
            "rev-a",
            runtime_state("rev-a", &["model-a"]),
            true,
        );
        assert!(
            status.ready,
            "a filtered-out disk model must not block chat"
        );
        assert!(status.synchronized);
        assert_eq!(validate_runtime_ready(&status, Some("model-a")), Ok(()));
        // The filtered model is still correctly refused for a direct request.
        assert!(validate_runtime_ready(&status, Some("model-hidden")).is_err());
    }

    #[test]
    fn upstream_branded_ids_are_hidden_from_the_frontend() {
        let legacy_model_46 = format!("{}-4.6", concat!("g", "rok"));
        let legacy_model_45 = format!("{}-4.5", concat!("g", "rok"));
        let status = auth_status_from_snapshots(
            vec!["model-a".into(), legacy_model_46.clone()],
            None,
            "rev-a",
            runtime_state(
                "rev-a",
                &[
                    "model-a",
                    legacy_model_46.as_str(),
                    legacy_model_45.as_str(),
                ],
            ),
            true,
        );
        assert_eq!(status.providers, vec!["model-a".to_string()]);
        assert_eq!(status.runtime_models, vec!["model-a".to_string()]);
        assert_eq!(
            status.unfiltered_runtime_models,
            vec!["model-a".to_string(), legacy_model_46, legacy_model_45,],
            "authorization must still see the Runtime catalog verbatim"
        );
    }

    /// The display filter must never lock chat: readiness is computed from the
    /// unfiltered catalog, so a catalog of only branded ids still reads ready and
    /// those ids remain sendable. Otherwise a bundled-catalog fallback would take
    /// the app from "usable with an odd model name" to "cannot send at all".
    #[test]
    fn branded_id_filter_does_not_regress_readiness_or_authorization() {
        let legacy_model = format!("{}-4.6", concat!("g", "rok"));
        let status = auth_status_from_snapshots(
            vec![legacy_model.clone()],
            None,
            "rev-a",
            runtime_state("rev-a", &[legacy_model.as_str()]),
            true,
        );
        assert!(status.ready, "a display filter must not block chat");
        assert!(status.runtime_ready);
        assert!(status.runtime_models.is_empty(), "nothing branded is shown");
        assert_eq!(validate_runtime_ready(&status, Some(&legacy_model)), Ok(()));
    }

    /// A user's own connection whose id happens to contain a brand token stays
    /// fully functional — only its rendered label is replaced.
    #[test]
    fn user_model_matching_a_brand_token_is_still_sendable() {
        let user_model = format!("my-{}-proxy", concat!("g", "rok"));
        let status = auth_status_from_snapshots(
            vec![user_model.clone()],
            None,
            "rev-a",
            runtime_state("rev-a", &[user_model.as_str()]),
            true,
        );
        assert!(status.ready);
        assert_eq!(validate_runtime_ready(&status, Some(&user_model)), Ok(()));
    }

    #[test]
    fn brand_token_matching_is_case_insensitive_and_substring_based() {
        let model_brand = concat!("g", "rok");
        let vendor_brand = concat!("x", "ai");
        let branded_ids = [
            format!("{model_brand}-4.6"),
            format!("{} 4.5", model_brand.to_ascii_uppercase()),
            vendor_brand.to_ascii_uppercase(),
            format!("{vendor_brand}-{model_brand}-build"),
            format!("{}/v1", concat!("x", ".", "ai")),
            format!("Space{vendor_brand}"),
        ];
        for id in &branded_ids {
            assert!(is_upstream_branded_model_id(id), "{id} must be filtered");
        }
        for id in [
            "gpt-4o",
            "deepseek-chat",
            "qwen-max",
            "claude-sonnet-4",
            "echo-agent-build",
        ] {
            assert!(!is_upstream_branded_model_id(id), "{id} must be kept");
        }
    }

    /// An empty catalog is the signal that the Runtime rejected the reload or
    /// filtered everything out; it must not read as ready.
    #[test]
    fn empty_runtime_catalog_blocks_readiness() {
        let status = auth_status_from_snapshots(
            vec!["model-a".into()],
            None,
            "rev-a",
            runtime_state("rev-a", &[]),
            true,
        );
        assert!(!status.ready);
        assert!(!status.runtime_ready);
        assert!(
            status.synchronized,
            "revision did match; only the catalog is empty"
        );
        assert!(status.reason.as_deref().unwrap().contains("可用范围"));
    }

    #[test]
    fn orphaned_session_is_adopted_only_for_a_matching_request() {
        let state = AppState::default();
        state.record_orphaned_session(OrphanedSession {
            session_id: "session-1".into(),
            cwd: "/work".into(),
            model_id: Some("model-a".into()),
            permission_mode: "auto".into(),
        });
        // Recording the same session twice must not duplicate it.
        state.record_orphaned_session(OrphanedSession {
            session_id: "session-1".into(),
            cwd: "/work".into(),
            model_id: Some("model-a".into()),
            permission_mode: "auto".into(),
        });

        assert_eq!(
            state.take_orphaned_session("/other", Some("model-a"), "auto"),
            None
        );
        assert_eq!(
            state.take_orphaned_session("/work", Some("model-b"), "auto"),
            None
        );
        assert_eq!(state.take_orphaned_session("/work", None, "auto"), None);
        assert_eq!(
            state.take_orphaned_session("/work", Some("model-a"), "ask"),
            None
        );
        assert_eq!(
            state.take_orphaned_session("/work", Some("model-a"), "auto"),
            Some("session-1".to_string())
        );
        // Adopted once only.
        assert_eq!(
            state.take_orphaned_session("/work", Some("model-a"), "auto"),
            None
        );
    }

    #[test]
    fn forged_session_cwd_is_rejected_without_expanding_filesystem_access() {
        let temp = tempfile::tempdir().unwrap();
        let trusted = temp.path().join("trusted");
        let forged = temp.path().join("forged");
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&forged).unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        filesystem
            .authorize_workspace(&trusted.to_string_lossy())
            .unwrap();

        let error = authorized_session_cwd(&filesystem, &forged.to_string_lossy()).unwrap_err();
        assert!(error.contains("未经用户授权"));
        // Validation is non-mutating: a second attempt is still rejected.
        assert!(authorized_session_cwd(&filesystem, &forged.to_string_lossy()).is_err());
    }

    #[test]
    fn native_selected_workspace_is_accepted_for_new_sessions() {
        let workspace = tempfile::tempdir().unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        let canonical = filesystem
            .authorize_workspace(&workspace.path().to_string_lossy())
            .unwrap();

        assert_eq!(
            authorized_session_cwd(&filesystem, &workspace.path().to_string_lossy()).unwrap(),
            canonical.to_string_lossy().into_owned()
        );
    }

    #[test]
    fn restored_trusted_workspace_is_accepted_for_loaded_sessions() {
        let workspace = tempfile::tempdir().unwrap();
        // `FilesystemAccess::new` feeds durable session/knowledge roots through
        // this same native-only registration method during startup.
        let restored = crate::shell_fs::FilesystemAccess::default();
        let persisted_canonical = restored
            .authorize_workspace(&workspace.path().to_string_lossy())
            .unwrap();

        assert_eq!(
            authorized_session_cwd(&restored, &workspace.path().to_string_lossy()).unwrap(),
            persisted_canonical.to_string_lossy().into_owned()
        );
    }

    #[test]
    fn bound_workspace_cannot_be_retargeted_even_when_both_roots_are_authorized() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        let bound = filesystem
            .authorize_workspace(&original.path().to_string_lossy())
            .unwrap();
        let other = filesystem
            .authorize_workspace(&other.path().to_string_lossy())
            .unwrap();
        let state = AppState::default();
        state
            .session_workspaces
            .lock()
            .unwrap()
            .insert("fixed-session".into(), bound.clone());
        assert_eq!(
            trusted_existing_session_cwd(&state, &filesystem, "fixed-session", None).unwrap(),
            bound
        );
        assert!(trusted_existing_session_cwd(
            &state,
            &filesystem,
            "fixed-session",
            Some(&other.to_string_lossy())
        )
        .is_err());
        std::fs::remove_dir(&bound).unwrap();
        assert!(trusted_existing_session_cwd(&state, &filesystem, "fixed-session", None).is_err());
        assert!(
            !bound.exists(),
            "validation must not recreate a missing directory"
        );
    }

    #[test]
    fn durable_workspace_is_restored_without_marking_an_unloaded_session_resident() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        let bound = filesystem
            .authorize_workspace(&original.path().to_string_lossy())
            .unwrap();
        let other = filesystem
            .authorize_workspace(&other.path().to_string_lossy())
            .unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let state = AppState::default();
        state.record_session_workspace(&id, &bound).unwrap();
        state.clear_session_workspaces();
        assert!(state.session_workspace(&id).is_err());
        assert_eq!(
            trusted_existing_session_cwd(&state, &filesystem, &id, Some(&bound.to_string_lossy()))
                .unwrap(),
            bound
        );
        assert!(trusted_existing_session_cwd(
            &state,
            &filesystem,
            &id,
            Some(&other.to_string_lossy())
        )
        .is_err());
        assert!(
            state.session_workspace(&id).is_err(),
            "validation must not activate a Runtime session"
        );
        crate::meta::clear_session_metadata(&id).unwrap();
    }

    #[test]
    fn history_management_keeps_its_identity_when_the_working_directory_disappears() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let filesystem = crate::shell_fs::FilesystemAccess::default();
        let bound = filesystem
            .authorize_workspace(&original.path().to_string_lossy())
            .unwrap();
        let state = AppState::default();
        let id = uuid::Uuid::now_v7().to_string();
        state.record_session_workspace(&id, &bound).unwrap();
        std::fs::remove_dir(&bound).unwrap();
        assert!(trusted_existing_session_cwd(&state, &filesystem, &id, None).is_err());
        for resident in [true, false] {
            if !resident {
                state.clear_session_workspaces();
            }
            assert_eq!(
                trusted_session_history_cwd(&state, &id, None).unwrap(),
                bound
            );
            assert_eq!(
                trusted_session_history_cwd(&state, &id, Some(&bound.to_string_lossy())).unwrap(),
                bound
            );
            assert!(trusted_session_history_cwd(
                &state,
                &id,
                Some(&other.path().to_string_lossy())
            )
            .is_err());
        }
        assert!(
            !bound.exists(),
            "history management must never recreate the directory"
        );
        crate::meta::clear_session_metadata(&id).unwrap();
    }

    #[test]
    fn legacy_history_uses_native_catalog_identity_without_requiring_a_live_directory() {
        let original = tempfile::tempdir().unwrap();
        let bound = original.path().canonicalize().unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let encoded =
            echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&bound.to_string_lossy());
        let record = crate::paths::echo_agent_home_dir()
            .join("sessions")
            .join(encoded)
            .join(&id);
        std::fs::create_dir_all(&record).unwrap();
        std::fs::write(
            record.join("summary.json"),
            serde_json::json!({"session_id":id,"cwd":bound}).to_string(),
        )
        .unwrap();
        std::fs::remove_dir(&bound).unwrap();
        let state = AppState::default();
        assert_eq!(
            trusted_session_history_cwd(&state, &id, None).unwrap(),
            bound
        );
        assert!(trusted_session_history_cwd(&state, &id, Some("/forged-workspace")).is_err());
        std::fs::remove_dir_all(&record).unwrap();
        assert!(trusted_session_history_cwd(&state, &id, None).is_err());
    }

    #[test]
    fn init_generation_guards_stale_cleanup() {
        let state = AppState::default();
        let first = state.begin_init_generation();
        let second = state.begin_init_generation();
        assert!(second > first);
        assert!(!state.is_current_generation(first));
        assert!(state.is_current_generation(second));

        // A superseded generation's failure cleanup must leave the current
        // runtime state untouched.
        let (client, _agent) = echo_agent_acp::acp_channels();
        *state.tx.lock().unwrap() = Some(client.tx.clone());
        state.mark_runtime_models_synced(
            &client.tx,
            "rev-a".into(),
            vec!["model-a".into()],
            "echoagent.api_key".into(),
        );
        clear_runtime_after_init_failure(&state, first);
        assert!(state.runtime_models.lock().unwrap().initialized);
        assert!(state.tx.lock().unwrap().is_some());

        clear_runtime_after_init_failure(&state, second);
        assert!(!state.runtime_models.lock().unwrap().initialized);
        assert!(state.tx.lock().unwrap().is_none());
    }

    #[test]
    fn expired_model_lease_requests_exactly_one_runtime_reload() {
        let state = AppState::default();
        let (channels, _agent) = echo_agent_acp::acp_channels();
        *state.tx.lock().unwrap() = Some(channels.tx.clone());
        state.mark_runtime_models_synced(
            &channels.tx,
            "revision-before-expiry".into(),
            vec!["managed-model".into()],
            "echoagent.api_key".into(),
        );

        state.mark_runtime_model_reload_pending();
        assert!(!state.runtime_models.lock().unwrap().initialized);
        assert!(state.take_runtime_model_reload_pending());
        assert!(!state.take_runtime_model_reload_pending());
    }

    /// A retired runtime's thread exit must not report the replacement as dead.
    #[test]
    fn agent_death_from_a_retired_runtime_is_not_reported() {
        let state = AppState::default();
        let (retired, _retired_agent) = echo_agent_acp::acp_channels();
        let (current, _current_agent) = echo_agent_acp::acp_channels();
        *state.tx.lock().unwrap() = Some(current.tx.clone());
        state.mark_runtime_models_synced(
            &current.tx,
            "rev-a".into(),
            vec!["model-a".into()],
            "echoagent.api_key".into(),
        );

        assert!(!state.mark_runtime_dead_if_current(&retired.tx, "old thread exited"));
        assert!(state.runtime_models.lock().unwrap().initialized);

        assert!(state.mark_runtime_dead_if_current(&current.tx, "current thread exited"));
        assert!(!state.runtime_models.lock().unwrap().initialized);
    }
}
