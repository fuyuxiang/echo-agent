//! Built-in Browser Use and Computer Use service.
//!
//! The embedded Runtime already consumes MCP and expands `${session_id}` in
//! HTTP headers. This module uses that trusted session binding rather than an
//! LLM-supplied argument, so automation state cannot cross task boundaries.

mod accessibility;
mod arguments;
mod artifact;
mod browser;
mod computer;
mod desktop_coordinator;
mod network_proxy;
pub(crate) mod task;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use browser::BrowserController;
use computer::ComputerController;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::RwLockReadGuard;
use tokio::sync::{oneshot, Mutex as AsyncMutex, RwLock as AsyncRwLock};
use tokio_util::sync::CancellationToken;

pub const MCP_SERVER_NAME: &str = "echoagent-automation";
pub const AUTH_HEADER: &str = "Authorization";
pub const SESSION_HEADER: &str = "X-EchoAgent-Session-Id";
const MAX_MCP_BODY_BYTES: usize = 256 * 1024;
const MAX_RPC_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 128 * 1024;
const MAX_SESSION_ID_CHARS: usize = 256;
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Physical writes require a live user-task execution lease. The original user
/// task authorizes continuous desktop execution; native target/focus/identity
/// checks still apply. There is no per-app, per-button or coordinate approval.
/// Runtime permission mode cannot manufacture this user-task authorization.
pub(crate) const DESKTOP_WRITE_TOOLS: &[&str] = &[
    "computer_click",
    "computer_click_control",
    "computer_drag",
    "computer_type",
    "computer_key",
    "computer_activate_window",
    "computer_key_window",
    "computer_file_dialog",
    "computer_open_app",
    "computer_invoke",
    "computer_set_value",
];

pub(crate) fn requires_desktop_consent(tool_name: &str) -> bool {
    DESKTOP_WRITE_TOOLS.contains(&tool_name)
}

static SERVICE: OnceLock<Arc<AutomationService>> = OnceLock::new();
static PERSISTED: Mutex<bool> = Mutex::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    Default,
    BrowserUse,
    ComputerUse,
}

impl AutomationMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" | "agent" => Ok(Self::Default),
            "browser_use" | "browser" => Ok(Self::BrowserUse),
            "computer_use" | "computer" => Ok(Self::ComputerUse),
            _ => Err(format!("unsupported automation mode: {value}")),
        }
    }

    fn allows_browser(self) -> bool {
        matches!(self, Self::BrowserUse)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationStatus {
    pub session_id: String,
    pub mode: AutomationMode,
    pub paused: bool,
    pub computer_authorized: bool,
    pub task: task::TaskExecution,
    pub allow_private_network: bool,
    pub browser: browser::BrowserCapability,
    pub computer: computer::ComputerCapability,
    pub browser_running: bool,
    pub browser_url: Option<String>,
    pub browser_title: Option<String>,
    pub browser_has_data: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationCapabilities {
    pub browser: browser::BrowserCapability,
    pub computer: computer::ComputerCapability,
}

struct AutomationSession {
    mode: Mutex<AutomationMode>,
    task: Mutex<task::TaskExecution>,
    intent_generation: std::sync::atomic::AtomicU64,
    allow_private_network: Mutex<bool>,
    browser: AsyncMutex<Option<BrowserController>>,
    computer: Mutex<ComputerController>,
    accessibility: Mutex<Option<Arc<accessibility::Controller>>>,
    action_token: Mutex<CancellationToken>,
    execution_gate: Arc<AsyncRwLock<()>>,
}

impl AutomationSession {
    fn commit_mode(&self, mode: AutomationMode, activate: bool) {
        let mut current = self.mode.lock().unwrap();
        let changed = *current != mode;
        *current = mode;
        if activate {
            self.task.lock().unwrap().resume(false);
        } else if changed && mode != AutomationMode::Default {
            self.task.lock().unwrap().pause("mode_changed");
        }
    }
    #[cfg(test)]
    fn new(mode: AutomationMode) -> Self {
        Self::with_scope(mode, None)
    }

    fn with_scope(mode: AutomationMode, scope: Option<String>) -> Self {
        // A restored task must never silently regain control after an app
        // restart. A deliberate mode switch starts active; restoring an
        // already-automated task starts paused until the user resumes in UI.
        let restored_automation = mode != AutomationMode::Default;
        Self {
            mode: Mutex::new(mode),
            task: Mutex::new(task::TaskExecution::new(restored_automation, scope)),
            intent_generation: std::sync::atomic::AtomicU64::new(0),
            allow_private_network: Mutex::new(false),
            browser: AsyncMutex::new(None),
            computer: Mutex::new(ComputerController::default()),
            accessibility: Mutex::new(None),
            action_token: Mutex::new(CancellationToken::new()),
            execution_gate: Arc::new(AsyncRwLock::new(())),
        }
    }

    fn action_token(&self) -> CancellationToken {
        self.action_token.lock().unwrap().clone()
    }

    fn cancel_actions(&self) {
        self.task.lock().unwrap().revoke();
        self.action_token.lock().unwrap().cancel();
    }

    fn authorize_computer_task(&self, token: &CancellationToken) -> Result<(), String> {
        if token.is_cancelled()
            || self.task.lock().unwrap().paused()
            || *self.mode.lock().unwrap() != AutomationMode::ComputerUse
        {
            return Err("任务已暂停或模式变化，请重新确认电脑操作授权".into());
        }
        self.task.lock().unwrap().authorized = true;
        Ok(())
    }

    fn computer_task_authorized(&self, token: &CancellationToken) -> bool {
        let mode = *self.mode.lock().unwrap();
        let task = self.task.lock().unwrap();
        !token.is_cancelled()
            && !task.paused()
            && mode == AutomationMode::ComputerUse
            && task.authorized
    }

    fn reset_action_token(&self) {
        self.task.lock().unwrap().revoke();
        let mut current = self.action_token.lock().unwrap();
        current.cancel();
        *current = CancellationToken::new();
    }
}

async fn enter_action<'a>(
    session: &'a AutomationSession,
    token: &CancellationToken,
    required: AutomationMode,
) -> Result<RwLockReadGuard<'a, ()>, String> {
    let guard = session.execution_gate.read().await;
    if token.is_cancelled() || session.task.lock().unwrap().paused() {
        return Err("自动化已暂停。用户接管期间不会执行任何浏览器或电脑操作。".into());
    }
    if *session.mode.lock().unwrap() != required {
        return Err("任务模式已变化，本次自动化操作已取消".into());
    }
    Ok(guard)
}

async fn native_lease(
    session: &AutomationSession,
    token: &CancellationToken,
) -> Result<accessibility::ExecutionLease, String> {
    let desktop = desktop_coordinator::acquire(token).await?;
    let gate = session.execution_gate.clone().read_owned().await;
    if token.is_cancelled()
        || session.task.lock().unwrap().paused()
        || *session.mode.lock().unwrap() != AutomationMode::ComputerUse
    {
        return Err("cancelled: native action stopped before dispatch".into());
    }
    Ok(accessibility::ExecutionLease {
        _desktop: desktop,
        _session: gate,
    })
}

async fn browser_preflight<T>(
    session: &AutomationSession,
    token: &CancellationToken,
    preparation: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let _guard = enter_action(session, token, AutomationMode::BrowserUse).await?;
    tokio::select! {
        biased;
        _ = token.cancelled() => Err("自动化已暂停，浏览器准备操作已取消".into()),
        result = preparation => result,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationApproval {
    pub request_id: String,
    pub session_id: String,
    pub tool: String,
    pub title: String,
    pub description: String,
    pub details: Value,
}

struct PendingApproval {
    request: AutomationApproval,
    response: oneshot::Sender<bool>,
    cancellation: CancellationToken,
}

struct BrowserApprovalPlan {
    title: String,
    description: String,
    details: Value,
    expected_url: String,
    expected_target: Option<(String, Value)>,
    interaction: Option<browser::BrowserInteraction>,
}

struct AutomationManager {
    app: AppHandle,
    sessions: AsyncMutex<HashMap<String, Arc<AutomationSession>>>,
    approvals: AsyncMutex<HashMap<String, PendingApproval>>,
}

struct AutomationService {
    manager: Arc<AutomationManager>,
    port: u16,
    token: String,
    ready: AtomicBool,
}

impl AutomationManager {
    fn new(app: AppHandle) -> Self {
        Self {
            app,
            sessions: AsyncMutex::new(HashMap::new()),
            approvals: AsyncMutex::new(HashMap::new()),
        }
    }

    async fn session(&self, session_id: &str) -> Result<Arc<AutomationSession>, String> {
        validate_session_id(session_id)?;
        let mut sessions = self.sessions.lock().await;
        Ok(sessions
            .entry(session_id.to_string())
            .or_insert_with(|| {
                let mode = crate::meta::agent_mode(session_id)
                    .as_deref()
                    .and_then(|mode| AutomationMode::parse(mode).ok())
                    .unwrap_or(AutomationMode::Default);
                Arc::new(AutomationSession::with_scope(
                    mode,
                    crate::meta::desktop_task_scope(session_id),
                ))
            })
            .clone())
    }

    async fn set_mode(&self, session_id: &str, mode: AutomationMode) -> Result<(), String> {
        self.session(session_id)
            .await?
            .intent_generation
            .fetch_add(1, Ordering::SeqCst);
        self.apply_mode(session_id, mode, true).await
    }

    async fn restore_mode(&self, session_id: &str, mode: AutomationMode) -> Result<(), String> {
        self.apply_mode(session_id, mode, false).await
    }

    async fn apply_mode(
        &self,
        session_id: &str,
        mode: AutomationMode,
        activate: bool,
    ) -> Result<(), String> {
        let session = self.session(session_id).await?;
        // Loading history is not a user request to resume automation. Keep
        // both paused sessions and already-running background tasks intact.
        if !activate && *session.mode.lock().unwrap() == mode {
            return Ok(());
        }
        session.cancel_actions();
        self.reject_session_approvals(session_id).await;
        let _exclusive = session.execution_gate.write().await;
        session.accessibility.lock().unwrap().take();
        session.computer.lock().unwrap().invalidate_frame();
        session.commit_mode(mode, activate);
        session.reset_action_token();
        if !mode.allows_browser() {
            if let Some(mut browser) = session.browser.lock().await.take() {
                let _ = browser.stop().await;
            }
        }
        self.emit_status(session_id).await;
        Ok(())
    }

    async fn status(&self, session_id: &str) -> Result<AutomationStatus, String> {
        let session = self.session(session_id).await?;
        let allow_private_network = *session.allow_private_network.lock().unwrap();
        let mut browser_guard = session.browser.lock().await;
        let browser_status = match browser_guard.as_mut() {
            Some(browser) => browser.status().await.ok(),
            None => None,
        };
        // Capture execution fields together after asynchronous provider queries.
        // Mixing a pre-await scope with a post-await grant misrepresents admission.
        let (mode, task) = {
            let mode = session.mode.lock().unwrap();
            (*mode, session.task.lock().unwrap().clone())
        };
        let paused = task.paused();
        let computer_authorized = task.authorized
            && !paused
            && mode == AutomationMode::ComputerUse
            && !session.action_token().is_cancelled();
        Ok(AutomationStatus {
            session_id: session_id.to_string(),
            mode,
            paused,
            computer_authorized,
            task,
            allow_private_network,
            browser: browser::capability(),
            computer: ComputerController::capability(),
            browser_running: browser_status.as_ref().is_some_and(|status| status.running),
            browser_url: browser_status
                .as_ref()
                .and_then(|status| status.url.clone()),
            browser_title: browser_status.and_then(|status| status.title),
            browser_has_data: browser::profile_exists(session_id),
        })
    }

    async fn emit_status(&self, session_id: &str) {
        if let Ok(status) = self.status(session_id).await {
            let _ = self.app.emit("automation://status", status);
        }
    }

    async fn ensure_active(
        &self,
        session_id: &str,
        required: AutomationMode,
    ) -> Result<Arc<AutomationSession>, String> {
        let session = self.session(session_id).await?;
        if *session.mode.lock().unwrap() != required {
            return Err(match required {
                AutomationMode::BrowserUse => {
                    "当前任务未开启操作网页，请从输入框的 + 菜单开启".into()
                }
                AutomationMode::ComputerUse => "请直接说明需要在电脑上完成的任务".into(),
                AutomationMode::Default => "当前任务模式不允许此操作".into(),
            });
        }
        if session.task.lock().unwrap().paused() {
            return Err("自动化已暂停。用户接管期间不会执行任何浏览器或电脑操作。".into());
        }
        Ok(session)
    }

    async fn pause(&self, session_id: &str, paused: bool) -> Result<(), String> {
        self.pause_with_reason(session_id, paused, "user_paused")
            .await
    }

    async fn pause_with_reason(
        &self,
        session_id: &str,
        paused: bool,
        reason: &str,
    ) -> Result<(), String> {
        let session = self.session(session_id).await?;
        if paused {
            session.intent_generation.fetch_add(1, Ordering::SeqCst);
            // Cancel first, then wait for the exclusive gate. Once this method
            // returns, no action admitted under the old token can still run.
            session.cancel_actions();
            session.task.lock().unwrap().pause(reason);
            // Taking over is a hard boundary: no earlier approval request may
            // remain live and resume an operation behind the user's back.
            self.reject_session_approvals(session_id).await;
            let _exclusive = session.execution_gate.write().await;
            // Reassert after the gate: a preflight already holding it may have
            // been finishing admission when cancellation arrived. Its token is
            // cancelled; the acknowledged lifecycle must stay paused as well.
            session.task.lock().unwrap().pause(reason);
            let worker = session.accessibility.lock().unwrap().clone();
            if let Some(worker) = worker {
                if worker.invalidate_elements().await.is_err() {
                    session.accessibility.lock().unwrap().take();
                }
            }
            session.computer.lock().unwrap().invalidate_frame();
        } else {
            let _exclusive = session.execution_gate.write().await;
            session.reset_action_token();
            session.task.lock().unwrap().resume(false);
        }
        self.emit_status(session_id).await;
        Ok(())
    }

    async fn stop(&self, session_id: &str) -> Result<(), String> {
        let session = self.session(session_id).await?;
        self.stop_existing(session_id, &session).await
    }

    async fn stop_existing(
        &self,
        session_id: &str,
        session: &Arc<AutomationSession>,
    ) -> Result<(), String> {
        session.intent_generation.fetch_add(1, Ordering::SeqCst);
        session.cancel_actions();
        session.task.lock().unwrap().stop();
        self.reject_session_approvals(session_id).await;
        let _exclusive = session.execution_gate.write().await;
        session.task.lock().unwrap().stop();
        session.accessibility.lock().unwrap().take();
        session.computer.lock().unwrap().invalidate_frame();
        let browser_result = stop_browser(&session.browser).await;
        self.emit_status(session_id).await;
        browser_result
    }

    async fn clear_browser_data(&self, session_id: &str) -> Result<(), String> {
        self.stop(session_id).await?;
        browser::remove_profile(session_id).await
    }

    async fn reject_session_approvals(&self, session_id: &str) {
        let mut approvals = self.approvals.lock().await;
        let request_ids = approvals
            .iter()
            .filter_map(|(id, pending)| {
                (pending.request.session_id == session_id).then_some(id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in request_ids {
            if let Some(pending) = approvals.remove(&request_id) {
                let _ = pending.response.send(false);
                let _ = self.app.emit(
                    "automation://approval-closed",
                    json!({ "requestId": request_id, "sessionId": session_id }),
                );
            }
        }
    }

    async fn set_private_network(&self, session_id: &str, allowed: bool) -> Result<(), String> {
        let session = self.session(session_id).await?;
        *session.allow_private_network.lock().unwrap() = allowed;
        if let Some(browser) = session.browser.lock().await.as_mut() {
            browser.set_allow_private_network(allowed);
            if !allowed {
                // If the user revokes access while a private page is open,
                // leave it immediately instead of waiting for the next tool.
                let _ = browser.enforce_current_url_policy().await;
            }
        }
        self.emit_status(session_id).await;
        Ok(())
    }

    async fn approve(
        &self,
        session_id: &str,
        tool: &str,
        title: &str,
        description: &str,
        details: Value,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        if cancellation.is_cancelled() {
            return Err("自动化已暂停，操作未进入确认阶段".into());
        }
        if tool.starts_with("computer_") && requires_desktop_consent(tool) {
            let session = self.sessions.lock().await.get(session_id).cloned();
            return if session.is_some_and(|session| session.computer_task_authorized(cancellation))
            {
                Ok(())
            } else {
                Err("电脑任务尚未开始或已暂停；请说明任务，或发送继续来续做原任务".into())
            };
        }
        let request_id = uuid::Uuid::now_v7().to_string();
        let request = AutomationApproval {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            details,
        };
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().await.insert(
            request_id.clone(),
            PendingApproval {
                request: request.clone(),
                response: tx,
                cancellation: cancellation.clone(),
            },
        );
        if self.app.emit("automation://approval", &request).is_err() {
            self.approvals.lock().await.remove(&request_id);
            return Err("无法显示自动化安全确认，操作已取消".into());
        }
        let result = tokio::select! {
            _ = cancellation.cancelled() => {
                Err("自动化已暂停，待确认操作已取消".into())
            }
            response = tokio::time::timeout(APPROVAL_TIMEOUT, rx) => match response {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err("用户拒绝了该高风险自动化操作".into()),
            Ok(Err(_)) => Err("自动化安全确认已关闭，操作未执行".into()),
            Err(_) => Err("自动化安全确认等待超时，操作未执行".into()),
            }
        };
        // The normal UI resolution path already removes the request and emits
        // this event. Cancellation and timeout have no UI caller, so close the
        // card here instead of leaving a stale decision on screen.
        if self.approvals.lock().await.remove(&request_id).is_some() {
            let _ = self.app.emit(
                "automation://approval-closed",
                json!({ "requestId": request_id, "sessionId": session_id }),
            );
        }
        result
    }

    async fn shutdown_all(&self) -> Result<(), String> {
        let sessions = self
            .sessions
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in &sessions {
            session.cancel_actions();
            session.task.lock().unwrap().pause("user_paused");
        }
        let pending = std::mem::take(&mut *self.approvals.lock().await);
        for (request_id, approval) in pending {
            let session_id = approval.request.session_id;
            let _ = approval.response.send(false);
            let _ = self.app.emit(
                "automation://approval-closed",
                json!({ "requestId": request_id, "sessionId": session_id }),
            );
        }
        let mut errors = Vec::new();
        for session in sessions {
            let _exclusive = session.execution_gate.write().await;
            session.accessibility.lock().unwrap().take();
            session.computer.lock().unwrap().invalidate_frame();
            if let Err(error) = stop_browser(&session.browser).await {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }

    async fn forget_session(&self, session_id: &str) {
        // Clone without holding the sessions mutex through process shutdown.
        let session = self.sessions.lock().await.get(session_id).cloned();
        if let Some(session) = session {
            session.cancel_actions();
            session.task.lock().unwrap().pause("user_paused");
            self.reject_session_approvals(session_id).await;
            let _exclusive = session.execution_gate.write().await;
            if let Err(error) = stop_browser(&session.browser).await {
                tracing::error!(%error, %session_id, "retaining deleted task browser for a cleanup retry");
                return;
            }
        }
        self.sessions.lock().await.remove(session_id);
        self.reject_session_approvals(session_id).await;
        if let Err(error) = browser::remove_profile(session_id).await {
            tracing::warn!(%error, %session_id, "failed to remove deleted task browser profile");
        }
    }
}

async fn stop_browser(
    browser: &tokio::sync::Mutex<Option<BrowserController>>,
) -> Result<(), String> {
    // Keep ownership until the process is confirmed stopped. Taking it before
    // the await would lose the retry handle on an error or shutdown timeout.
    let mut browser = browser.lock().await;
    if let Some(controller) = browser.as_mut() {
        controller.stop().await?;
    }
    browser.take();
    Ok(())
}

fn manager() -> Result<&'static Arc<AutomationManager>, String> {
    let service = SERVICE
        .get()
        .ok_or_else(|| "自动化服务尚未启动".to_string())?;
    if !service.ready.load(Ordering::Acquire) {
        return Err(automation_service_unavailable_reason());
    }
    Ok(&service.manager)
}

fn automation_service_unavailable_reason() -> String {
    "自动化服务未就绪，请重启 EchoAgent 后重试".to_string()
}

pub fn serve(app: AppHandle) -> Result<(), String> {
    if let Some(service) = SERVICE.get() {
        return if service.ready.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(automation_service_unavailable_reason())
        };
    }
    let listener = bind_loopback()
        .ok_or_else(|| "automation MCP: failed to bind loopback socket".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("automation MCP: failed to read bound address: {error}"))?
        .port();
    let token = uuid::Uuid::now_v7().to_string();
    let authorization = format!("Bearer {token}");
    let service = Arc::new(AutomationService {
        manager: Arc::new(AutomationManager::new(app)),
        port,
        token,
        ready: AtomicBool::new(false),
    });
    if SERVICE.set(service.clone()).is_err() {
        return Err("automation MCP: process state was initialized twice".to_string());
    }

    // Tauri invokes setup on the native event-loop thread, which is not entered
    // into Tokio's IO runtime. Keep both std -> Tokio registration and Axum on
    // Tauri's runtime. A bounded startup acknowledgement prevents sessions from
    // observing an endpoint that has not actually been registered yet.
    let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
    let task_service = service.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match to_tokio_listener(listener).await {
            Ok(listener) => listener,
            Err(error) => {
                let message = format!("automation MCP: failed to register listener: {error}");
                let _ = startup_tx.send(Err(message));
                return;
            }
        };
        let expected_host = format!("127.0.0.1:{port}");
        let router = Router::new()
            .route(
                "/mcp",
                post(handle_post)
                    .get(method_not_allowed)
                    .delete(method_not_allowed),
            )
            .layer(DefaultBodyLimit::max(MAX_MCP_BODY_BYTES))
            .with_state(ServerState {
                authorization,
                expected_host,
                manager: task_service.manager.clone(),
            });

        task_service.ready.store(true, Ordering::Release);
        let _ = startup_tx.send(Ok(()));
        tracing::info!(port, "automation MCP server listening");
        if let Err(error) = axum::serve(listener, router).await {
            tracing::error!(%error, "automation MCP server stopped");
        }
        task_service.ready.store(false, Ordering::Release);
        if let Ok(mut persisted) = PERSISTED.lock() {
            *persisted = false;
        }
    });

    match startup_rx.recv_timeout(SERVER_START_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("automation MCP: startup task stopped unexpectedly".to_string())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(
                timeout_seconds = SERVER_START_TIMEOUT.as_secs(),
                "automation MCP startup acknowledgement timed out; continuing in degraded mode"
            );
            Ok(())
        }
    }
}

fn bind_loopback() -> Option<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).ok()
}

async fn to_tokio_listener(listener: TcpListener) -> std::io::Result<tokio::net::TcpListener> {
    listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(listener)
}

pub fn server_url() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("http://127.0.0.1:{}/mcp", service.port))
    })
}

pub fn authorization_header() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("Bearer {}", service.token))
    })
}

pub async fn set_session_mode(session_id: &str, mode: AutomationMode) -> Result<(), String> {
    validate_mode_capability(mode)?;
    manager()?.set_mode(session_id, mode).await
}

pub async fn restore_session_mode(session_id: &str, mode: AutomationMode) -> Result<(), String> {
    validate_mode_capability(mode)?;
    manager()?.restore_mode(session_id, mode).await
}

pub fn validate_mode_capability(mode: AutomationMode) -> Result<(), String> {
    match mode {
        AutomationMode::Default => Ok(()),
        AutomationMode::BrowserUse => {
            let capability = browser::capability();
            if capability.available {
                Ok(())
            } else {
                Err(capability
                    .reason
                    .unwrap_or_else(|| "操作网页当前不可用".into()))
            }
        }
        AutomationMode::ComputerUse => {
            let capability = ComputerController::capability();
            if capability.available {
                Ok(())
            } else {
                Err(capability
                    .reason
                    .unwrap_or_else(|| "操作电脑当前不可用".into()))
            }
        }
    }
}

pub async fn forget_session(session_id: &str) {
    if let Some(service) = SERVICE.get() {
        service.manager.forget_session(session_id).await;
    }
}

pub async fn pause_session(session_id: &str) -> Result<(), String> {
    manager()?.pause(session_id, true).await
}

pub async fn stop_session(session_id: &str) -> Result<(), String> {
    manager()?.stop(session_id).await
}

pub async fn stop_existing_session(session_id: &str) -> Result<(), String> {
    if let Some(service) = SERVICE.get() {
        let session = service
            .manager
            .sessions
            .lock()
            .await
            .get(session_id)
            .cloned();
        if let Some(session) = session {
            service.manager.stop_existing(session_id, &session).await?;
        }
    }
    Ok(())
}

pub async fn shutdown_all() -> Result<(), String> {
    if let Some(service) = SERVICE.get() {
        service.manager.shutdown_all().await?;
    }
    Ok(())
}

#[tauri::command]
pub fn automation_capabilities() -> AutomationCapabilities {
    let mut capabilities = AutomationCapabilities {
        browser: browser::capability(),
        computer: ComputerController::capability(),
    };
    let ready = SERVICE
        .get()
        .is_some_and(|service| service.ready.load(Ordering::Acquire));
    if !ready {
        let reason = automation_service_unavailable_reason();
        capabilities.browser.available = false;
        capabilities.browser.reason = Some(reason.clone());
        capabilities.computer.available = false;
        capabilities.computer.reason = Some(reason);
    }
    capabilities
}

/// Admission for a real user message only. This function is deliberately not an
/// MCP tool: neither the model nor document/tool content can grant or resume.
pub(crate) async fn prepare_user_prompt(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: &str,
    user_text: &str,
    has_attachments: bool,
    send_now: bool,
) -> Result<Option<u64>, String> {
    let mut intent = task::user_intent_for_selected_mode(
        user_text,
        has_attachments,
        crate::meta::agent_mode(session_id).as_deref() == Some("browser_use"),
    );
    if intent == task::UserIntent::Ordinary {
        // Replacing a model turn must also revoke its physical input lease,
        // including corrections such as "不要保存，先解释". Runtime cancellation
        // alone can drop an HTTP caller while a native worker is still running.
        if send_now
            && matches!(
                crate::meta::agent_mode(session_id).as_deref(),
                Some("computer_use" | "browser_use")
            )
        {
            manager()?
                .pause_with_reason(session_id, true, "user_instruction")
                .await?;
        }
        return Ok(None);
    }
    if intent != task::UserIntent::DesktopTask
        && !matches!(
            crate::meta::agent_mode(session_id).as_deref(),
            Some("computer_use" | "browser_use")
        )
    {
        return Ok(None);
    }
    let manager = manager()?;
    let session = manager.session(session_id).await?;
    let mode = *session.mode.lock().unwrap();
    let current = session.task.lock().unwrap().clone();
    // Manual mode selection supplies the app context for a first imperative
    // such as "新建文稿". It never authorizes discussion or a restored history.
    if intent == task::UserIntent::TaskUpdate
        && current.scope.is_none()
        && mode == AutomationMode::ComputerUse
        && !current.paused()
    {
        intent = task::UserIntent::DesktopTask;
    }
    let scope = task::updated_scope(current.scope.as_deref(), intent, user_text)?;
    if intent != task::UserIntent::DesktopTask && mode != AutomationMode::ComputerUse {
        if intent != task::UserIntent::Continue || mode != AutomationMode::BrowserUse {
            return Ok(None);
        }
    } else if scope.is_none() {
        return Err("当前没有可续做的电脑任务，请直接说明需要在哪个应用中完成什么".into());
    }
    let generation = session.intent_generation.load(Ordering::SeqCst);
    if intent == task::UserIntent::DesktopTask {
        validate_mode_capability(AutomationMode::ComputerUse)?;
        if mode != AutomationMode::ComputerUse {
            crate::agent_runtime::set_session_mode_id(tx, session_id, "computer_use")
                .await
                .map_err(|e| e.to_string())?;
            manager
                .apply_mode(session_id, AutomationMode::ComputerUse, true)
                .await?;
        }
    }
    let exclusive = session.execution_gate.write().await;
    if generation != session.intent_generation.load(Ordering::SeqCst) {
        session.cancel_actions();
        session.task.lock().unwrap().pause("user_paused");
        return Err("任务已被暂停，本条执行请求已取消；发送继续可从现场续做".into());
    }
    let computer_mode = *session.mode.lock().unwrap() == AutomationMode::ComputerUse;
    if computer_mode {
        // Commit descriptions only after Runtime accepts the mode. No token or
        // authorization is persisted, including when admission later fails.
        if let Some(scope) = &scope {
            crate::meta::set_desktop_task_context(session_id, scope)?;
        }
    }
    session.reset_action_token();
    if intent == task::UserIntent::DesktopTask {
        session.accessibility.lock().unwrap().take();
    } else {
        let worker = session.accessibility.lock().unwrap().clone();
        if let Some(worker) = worker {
            if worker.invalidate_elements().await.is_err() {
                session.accessibility.lock().unwrap().take();
            }
        }
    }
    session.computer.lock().unwrap().invalidate_frame();
    let admitted_generation = {
        let mut task = session.task.lock().unwrap();
        if let Some(scope) = scope {
            task.scope = Some(scope);
        }
        task.resume(computer_mode);
        task.generation
    };
    drop(exclusive);
    tracing::info!(%session_id, ?intent, admitted_generation, "trusted user task admitted");
    manager.emit_status(session_id).await;
    Ok(Some(admitted_generation))
}

pub(crate) async fn prompt_admission_failed(session_id: &str, admitted_generation: u64) {
    let Ok(manager) = manager() else { return };
    let Ok(session) = manager.session(session_id).await else {
        return;
    };
    let exclusive = session.execution_gate.write().await;
    {
        let task = session.task.lock().unwrap();
        // PromptRequest settles after the whole turn. A late failure of an old
        // turn must never pause a newer user task or undo an acknowledged stop.
        if task.paused() || task.generation != admitted_generation {
            return;
        }
    }
    session.intent_generation.fetch_add(1, Ordering::SeqCst);
    session.cancel_actions();
    session.task.lock().unwrap().pause("prompt_failed");
    manager.reject_session_approvals(session_id).await;
    let worker = session.accessibility.lock().unwrap().clone();
    if let Some(worker) = worker {
        if worker.invalidate_elements().await.is_err() {
            session.accessibility.lock().unwrap().take();
        }
    }
    session.computer.lock().unwrap().invalidate_frame();
    drop(exclusive);
    manager.emit_status(session_id).await;
}

#[tauri::command]
pub async fn automation_status(session_id: String) -> Result<AutomationStatus, String> {
    manager()?.status(&session_id).await
}

#[tauri::command]
pub async fn automation_clear_browser_data(session_id: String) -> Result<AutomationStatus, String> {
    manager()?.clear_browser_data(&session_id).await?;
    manager()?.status(&session_id).await
}

#[tauri::command]
pub async fn automation_pause(
    session_id: String,
    reason: Option<String>,
) -> Result<AutomationStatus, String> {
    let reason = match reason.as_deref() {
        None | Some("user_paused") => "user_paused",
        Some("validation_watchdog") => "validation_watchdog",
        _ => return Err("不支持的任务暂停原因".into()),
    };
    manager()?
        .pause_with_reason(&session_id, true, reason)
        .await?;
    manager()?.status(&session_id).await
}

#[tauri::command]
pub async fn automation_resume(session_id: String) -> Result<AutomationStatus, String> {
    manager()?.pause(&session_id, false).await?;
    manager()?.status(&session_id).await
}

#[tauri::command]
pub async fn automation_stop(session_id: String) -> Result<AutomationStatus, String> {
    manager()?.stop(&session_id).await?;
    manager()?.status(&session_id).await
}

#[tauri::command]
pub async fn automation_set_private_network(
    session_id: String,
    allowed: bool,
) -> Result<AutomationStatus, String> {
    if allowed {
        let approval_token = CancellationToken::new();
        manager()?
            .approve(
                &session_id,
                "automation_set_private_network",
                "允许访问本机和内网",
                "受控浏览器将可以访问环回地址、局域网和企业内网。网页可能接触本机服务，请确认当前任务确实需要此权限。",
                json!({ "allowed": true }),
                &approval_token,
            )
            .await?;
    }
    manager()?.set_private_network(&session_id, allowed).await?;
    manager()?.status(&session_id).await
}

#[tauri::command]
pub fn automation_request_computer_permissions() -> Result<computer::ComputerCapability, String> {
    ComputerController::request_permissions()
}

#[tauri::command]
pub async fn automation_pending_approvals(
    session_id: Option<String>,
) -> Result<Vec<AutomationApproval>, String> {
    let approvals = manager()?.approvals.lock().await;
    Ok(approvals
        .values()
        .filter(|pending| {
            session_id
                .as_deref()
                .is_none_or(|session_id| pending.request.session_id == session_id)
        })
        .map(|pending| pending.request.clone())
        .collect())
}

#[tauri::command]
pub async fn automation_resolve_approval(
    request_id: String,
    approved: bool,
    allow_for_task: Option<bool>,
) -> Result<bool, String> {
    let pending = manager()?.approvals.lock().await.remove(&request_id);
    let Some(pending) = pending else {
        return Ok(false);
    };
    let authorizing_session = if approved && allow_for_task.unwrap_or(false) {
        manager()?
            .sessions
            .lock()
            .await
            .get(&pending.request.session_id)
            .cloned()
    } else {
        None
    };
    let grant = if approved && allow_for_task.unwrap_or(false) {
        if !pending.request.tool.starts_with("computer_")
            || !requires_desktop_consent(&pending.request.tool)
        {
            Err("本任务授权仅适用于电脑操作确认".to_string())
        } else if let Some(session) = authorizing_session {
            let _exclusive = session.execution_gate.write().await;
            session.authorize_computer_task(&pending.cancellation)
        } else {
            Err("任务已结束，授权未生效".to_string())
        }
    } else {
        Ok(())
    };
    let _ = pending.response.send(approved && grant.is_ok());
    let _ = manager()?.app.emit(
        "automation://approval-closed",
        json!({ "requestId": request_id, "sessionId": pending.request.session_id }),
    );
    grant?;
    manager()?.emit_status(&pending.request.session_id).await;
    Ok(true)
}

/// Runtime's generic MCP prompt is delegated only to this process's registered
/// computer service. It still enforces its own task/one-action consent and
/// target validation. No model-controlled flag can create this authorization.
pub(crate) async fn manages_computer_permissions(session_id: &str) -> bool {
    let Some(service) = SERVICE.get() else {
        return false;
    };
    if !service.ready.load(Ordering::SeqCst) {
        return false;
    }
    service
        .manager
        .sessions
        .lock()
        .await
        .get(session_id)
        .is_some_and(|s| *s.mode.lock().unwrap() == AutomationMode::ComputerUse)
}

pub(crate) fn is_managed_computer_tool(name: &str) -> bool {
    (name.starts_with("computer_")
        || matches!(
            name,
            "automation_status" | "automation_pause" | "automation_stop"
        ))
        && tools_list_result()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == name)
}

#[derive(Clone)]
struct ServerState {
    authorization: String,
    expected_host: String,
    manager: Arc<AutomationManager>,
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn method_not_allowed() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn handle_post(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let session_id = match validate_headers(&headers, &state) {
        Ok(session_id) => session_id,
        Err(status) => return status.into_response(),
    };
    if body.len() > MAX_MCP_BODY_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => return rpc_error(Value::Null, -32700, format!("parse error: {error}")),
    };
    let Some(id) = request.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    if !matches!(id, Value::String(_) | Value::Number(_)) {
        return rpc_error(Value::Null, -32600, "invalid JSON-RPC id".into());
    }
    let result = match request.method.as_str() {
        "initialize" => Ok(initialize_result(&request.params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => tools_call(&state.manager, &session_id, &request.params).await,
        other => return rpc_error(id, -32601, format!("method not found: {other}")),
    };
    match result {
        Ok(result) => rpc_result(id, result),
        Err(error) => rpc_result(id, tool_error_result(&error)),
    }
}

fn validate_headers(headers: &HeaderMap, state: &ServerState) -> Result<String, StatusCode> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !constant_time_eq(authorization.as_bytes(), state.authorization.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !host.eq_ignore_ascii_case(&state.expected_host) {
        return Err(StatusCode::MISDIRECTED_REQUEST);
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let session_id = headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    validate_session_id(session_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(session_id.to_string())
}

fn initialize_result(params: &Value) -> Value {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-03-26");
    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": MCP_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tools_list_result() -> Value {
    let mut result = json!({ "tools": [
        tool("automation_status", "Read Browser Use and Computer Use capability, mode and lifecycle status for this task.", object_schema(&[])),
        tool("automation_pause", "Pause all browser/computer actions so the user can take over safely.", object_schema(&[])),
        tool("automation_stop", "Stop the controlled browser and all automation for this task.", object_schema(&[])),

        tool("browser_start", "Start the task-isolated controlled browser. Call before other browser tools.", object_schema(&[])),
        tool("browser_navigate", "Navigate the controlled browser to an absolute public http/https URL and wait for DOM readiness.", schema(json!({ "url": {"type":"string","maxLength":4096} }), &["url"])),
        tool("browser_snapshot", "Return current page title, URL, readable text and interactive elements with stable element refs.", object_schema(&[])),
        tool("browser_screenshot", "Capture the visible controlled-browser viewport. Returns an image and viewport metadata.", object_schema(&[])),
        tool("browser_click", "Click an element ref from browser_snapshot, or viewport coordinates. Every click requires independent user confirmation.", schema(json!({
            "elementRef":{"type":"string","maxLength":128}, "x":{"type":"number"}, "y":{"type":"number"},
            "button":{"type":"string","enum":["left","right","middle"],"default":"left"}, "clickCount":{"type":"integer","minimum":1,"maximum":3,"default":1}
        }), &[])),
        tool("browser_hover", "Move the browser pointer over an element ref.", schema(json!({"elementRef":{"type":"string","maxLength":128}}), &["elementRef"])),
        tool("browser_type", "Fill an input/contenteditable after confirmation and verify its actual value and validity. Use complete ISO values for date (YYYY-MM-DD), time (HH:mm), datetime-local, month or week; replace=true is required for these and numeric fields. Unsupported, readonly and invalid fields return an error. After an error inspect a fresh snapshot rather than repeating the same input. Password fields require manual takeover.", schema(json!({
            "elementRef":{"type":"string","maxLength":128}, "text":{"type":"string","maxLength":65536}, "replace":{"type":"boolean","default":true}
        }), &["elementRef","text"])),
        tool("browser_select", "Select one or more values/text labels in a select element after user confirmation.", schema(json!({
            "elementRef":{"type":"string","maxLength":128}, "values":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"string","maxLength":1024}}
        }), &["elementRef","values"])),
        tool("browser_upload", "Upload workspace files through a file input. This always pauses for user confirmation before any file is disclosed to the website.", schema(json!({
            "elementRef":{"type":"string","maxLength":128}, "paths":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string","maxLength":4096}}
        }), &["elementRef","paths"])),
        tool("browser_key", "Press one browser key with optional modifiers after user confirmation.", schema(json!({
            "key":{"type":"string","maxLength":64}, "modifiers":{"type":"array","maxItems":4,"items":{"type":"string","enum":["shift","ctrl","control","alt","option","cmd","command","meta"]}}
        }), &["key"])),
        tool("browser_scroll", "Scroll the current browser viewport by pixel deltas.", schema(json!({"deltaX":{"type":"number","default":0},"deltaY":{"type":"number"}}), &["deltaY"])),
        tool("browser_drag", "Drag between two viewport coordinates after user confirmation.", schema(json!({"fromX":{"type":"number"},"fromY":{"type":"number"},"toX":{"type":"number"},"toY":{"type":"number"}}), &["fromX","fromY","toX","toY"])),
        tool("browser_tabs", "List browser tabs or create/select/close one.", schema(json!({
            "action":{"type":"string","enum":["list","new","select","close"],"default":"list"}, "targetId":{"type":"string","maxLength":256}, "url":{"type":"string","maxLength":4096}
        }), &[])),
        tool("browser_wait", "Wait briefly for an asynchronous page update, then return the current URL.", schema(json!({"milliseconds":{"type":"integer","minimum":50,"maximum":30000,"default":1000}}), &[])),
        tool("browser_downloads", "List files downloaded by this task's isolated browser, including completion status and local path.", object_schema(&[])),
        tool("browser_stop", "Stop the controlled browser for this task without changing conversation mode.", object_schema(&[])),

        tool("computer_capabilities", "Check operating-system screen capture and input-control permissions.", object_schema(&[])),
        tool("computer_displays", "List display system dimensions. Captured image resolution can differ: use screenshot imageWidth/imageHeight and its frameId for coordinate actions.", object_schema(&[])),
        tool("computer_screenshot", "Capture a display. Returns a frameId, actual imageWidth/imageHeight, logical desktop bounds and PNG image. Action x/y are pixels in this captured image; the backend maps to logical desktop coordinates. Do not use computer_displays pixelWidth to scale the image. Capture immediately before a coordinate action.", schema(json!({"displayId":{"type":"string","maxLength":128}}), &[])),
        tool("computer_move", "Move the pointer using coordinates from a current computer_screenshot frame. Hover UI may change, so the frame is invalidated.", schema(json!({"frameId":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"}}), &["frameId","x","y"])),
        tool("computer_click", "Click under the active user task. Prefer ref for an observed native control: the backend obtains its current bounds and verifies the native hit target, with no screenshot coordinates. Set clickCount=2 to open a directory suggestion. Native ref clicks support the left button and counts 1 or 2 on macOS. If native clicking is unsupported or no control is exposed, use frameId/x/y from a fresh screenshot; x/y are captured-image pixels, never raw AX bounds. Choose exactly one target representation. No per-step approval is needed.", schema(json!({
            "ref":{"type":"string"},"frameId":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"},"button":{"type":"string","enum":["left","right","middle"],"default":"left"},"clickCount":{"type":"integer","minimum":1,"maximum":3,"default":1}
        }), &[])),
        tool("computer_drag", "Drag under the active user task using two captured-image pixel coordinates, not AX logical bounds. Consume the fresh screenshot frame.", schema(json!({
            "frameId":{"type":"string"},"fromX":{"type":"number"},"fromY":{"type":"number"},"toX":{"type":"number"},"toY":{"type":"number"},"durationMs":{"type":"integer","minimum":100,"maximum":5000,"default":500}
        }), &["frameId","fromX","fromY","toX","toY"])),
        tool("computer_scroll", "Scroll the active desktop target using a current screenshot frame. Positive deltaY scrolls down and the frame is invalidated.", schema(json!({"frameId":{"type":"string"},"deltaX":{"type":"integer","default":0},"deltaY":{"type":"integer"}}), &["frameId","deltaY"])),
        tool("computer_type", "Type into the focused desktop control under the active user task. Prefer native computer_set_value with inputMethod=keyboard for an observed text control.", schema(json!({
            "frameId":{"type":"string"},"text":{"type":"string","maxLength":65536}
        }), &["frameId","text"])),
        tool("computer_key", "Press a desktop key with optional modifiers under the active user task. Prefer computer_key_window when a native window reference is available.", schema(json!({
            "frameId":{"type":"string"},"key":{"type":"string","maxLength":64},"modifiers":{"type":"array","maxItems":4,"items":{"type":"string"}}
        }), &["frameId","key"])),
        tool("computer_wait", "Wait briefly before taking the next screenshot.", schema(json!({"milliseconds":{"type":"integer","minimum":50,"maximum":30000,"default":1000}}), &[]))
    ] });
    result["tools"]
        .as_array_mut()
        .unwrap()
        .extend(accessibility_tools());
    if let Some(click) = result["tools"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|tool| tool["name"] == "computer_click")
    {
        click["inputSchema"]["oneOf"] = json!([
            {"required":["ref"]},
            {"required":["frameId","x","y"]}
        ]);
    }
    result
}

fn accessibility_tools() -> Vec<Value> {
    vec![
        tool("computer_context", "Start here for a known app: resolve its localized name, executable/bundle alias or identifier, discover its windows and return compact current native controls and actual menu keyboard commands in ONE call. Supply app (for example the user's app name), appRef or windowRef. Focused modal dialogs are observed instead of their background file browser. Native actions already return updated state with fresh refs: use that state directly; do not repeat discovery/snapshot/screenshot unless the needed target is missing/truncated or layout is required; unrelated metadata errors do not require rereading the tree. Application/window refs are revalidated native identities, not execution authority. commands are observed shortcuts, never guessed app recipes.", schema(json!({"app":{"type":"string"},"appRef":{"type":"string"},"windowRef":{"type":"string"}}),&[])),
        tool("computer_click_control", "Click or double-click an observed native control ref without screenshot coordinates. Prefer this for observed rows/cells/buttons lacking an open/press action, including Go to Folder suggestions. The backend activates its original window, obtains current native bounds and hit-tests that the point is this retained control or its child; it never trusts model coordinates. Set clickCount=2 to open a directory suggestion. macOS supports hit-tested control clicking; unsupported providers/platforms return an explicit error for screenshot fallback. No per-step approval under the active task.", schema(json!({"ref":{"type":"string"},"clickCount":{"type":"integer","minimum":1,"maximum":2,"default":1}}), &["ref"])),
        tool("computer_verify_file", "Read-only verification after GUI saving: check an explicit absolute output path from the current user's task. Does not create, rename or write files. Returns exists=false when a path-like window title is not an actual file. Optional expectedText compares exact UTF-8 content including final newlines. Use expectedTrailingNewlines to declare boundary newlines as an integer if the provider strips string endings; this checks caller expectation, so also compare it with the user's original requirement.", schema(json!({"path":{"type":"string","maxLength":4096},"expectedText":{"type":"string","maxLength":64000},"expectedTrailingNewlines":{"type":"integer","minimum":0,"maximum":16,"description":"Explicit terminal LF count; use with expectedText without final LF when the provider strips string endings. Omit to preserve the expectedText exactly."}}), &["path"])),
        tool("computer_open_app", "Open an installed .app bundle (macOS) or .exe (Windows) by absolute appPath, under active task authorization. No arguments or shell commands are accepted. This may activate an already-running app: it does NOT create a new document. Verify running state, then separately create a new document if the task requires one.", schema(json!({"appPath":{"type":"string","maxLength":4096}}), &["appPath"])),
        tool("computer_list_apps", "Discover running native applications; optionally filter by exact observed/authorized name or identifier to locate a known target without reading unrelated pages. Returns session-bound app refs and matchStatus for filtered discovery; do not choose the first ambiguous match. Does not launch applications. Consume nextCursor pages when listing all applications.", schema(json!({"name":{"type":"string","minLength":1,"maxLength":512},"identifier":{"type":"string","minLength":1,"maxLength":1024}}), &[])),
        tool("computer_continue", "Read the next page from nextCursor until null. The opaque cursor is session-bound, single-use, expires after 120s, and becomes stale when its window snapshot changes. Page truncation differs from unavailable native content.", schema(json!({"cursor":{"type":"string"}}), &["cursor"])),
        tool("computer_list_windows", "Discover windows and foreground state, preferably scoped to the target appRef. Save/Open/Go to Folder sheets are often attached to the original document window, not separate windows: inspect that original window with computer_snapshot or computer_find even when its title and this list do not change. Returns window refs and documentUrl when the provider exposes a local document URL. A title containing a path may be a literal filename: verify documentUrl after saving. Inaccessible apps/providers preserve readable windows with status=incomplete, issues and truncated=true. An incomplete global listing does not mean the target has no windows; retry with its appRef.", schema(json!({"appRef":{"type":"string"}}), &[])),
        tool("computer_activate_window", "Activate an observed app/window under active task authorization and verify foreground identity.", schema(json!({"ref":{"type":"string"}}), &["ref"])),
        tool("computer_key_window", "Press a known key/shortcut in an observed native window under active task authorization. Use an actually observed menu shortcut, for example shortcut=cmd+s. Do not guess Save As shortcuts. Alternatively use key=n and modifiers=[cmd]. Provide exactly one representation; never put command+n in key. Revalidates the retained window, restores and verifies foreground focus, rejects protected keyboard focus, then dispatches under the desktop lease. Use for known dialog/menu shortcuts when native actions are unavailable. Returns updated state with current controls and menu commands. Use that state directly, without redundant discovery or screenshot; dispatch alone does not claim save/task completion. Coordinate tools still require a fresh frameId.", schema(json!({"ref":{"type":"string"},"shortcut":{"type":"string","minLength":1,"maxLength":80,"description":"Observed menu shortcut such as cmd+n or cmd+s. Do not also pass key/modifiers."},"key":{"type":"string","maxLength":64},"modifiers":{"type":"array","maxItems":4,"items":{"type":"string","enum":["shift","ctrl","control","alt","option","cmd","command","meta"]}}}), &["ref"])),
        tool("computer_file_dialog", "Prepare an ALREADY OPEN standard OS Open/Save panel for an absolute file path using native UI, in one cancellable operation. Currently macOS system panels only; all host apps use the same implementation. Does not open/reopen any panel, write files, or press its final Open/Save/Upload button. For Save, navigates the parent directory and fills only the basename; for Open, navigates to/selects the exact path. Path text readback and return to the original panel are checked. Use its returned state and press the actual final button once, then verify the outcome; never repeat on result_unknown. If no system panel is open, use the application actual observed command first.", schema(json!({"ref":{"type":"string","description":"Original document window ref, not the sheet/control ref"},"path":{"type":"string","maxLength":4096}}), &["ref","path"])),
        tool("computer_snapshot", "Read a bounded native control tree and provider documentUrl (when available). On macOS a focused modal sheet is the observation root, so Go to Folder controls are not buried in the parent file browser. windowRef remains the original document identity; focusedDialog and observationRootRole describe this scope. Supply windowRef (not ref). Failed child providers preserve readable nodes and report incomplete issues. Unchanged native targets retain short refs; changed, removed or expired targets lose their refs. Truncation/virtual content is not complete history. Bounds are desktop logical points on macOS, physical pixels on Windows.", schema(json!({"windowRef":{"type":"string"},"maxDepth":{"type":"integer","minimum":1,"maximum":20},"maxNodes":{"type":"integer","minimum":1,"maximum":1000},"maxText":{"type":"integer","minimum":1,"maximum":64000}}), &["windowRef"])),
        tool("computer_find", "Refresh the window tree and find by exact native role/name/identifier. Returns unique, ambiguous or not_found plus candidates. Never choose the first ambiguous match.", schema(json!({"windowRef":{"type":"string"},"role":{"type":"string"},"name":{"type":"string"},"identifier":{"type":"string"},"maxDepth":{"type":"integer"},"maxNodes":{"type":"integer"}}), &["windowRef"])),
        tool("computer_invoke", "Perform an advertised native action under active task authorization. Returns updated state with fresh controls; use it directly to verify the next step. An executed action alone does not verify the user's outcome. Never replay result_unknown blindly.", schema(json!({"ref":{"type":"string"},"action":{"type":"string"}}), &["ref","action"])),
        tool("computer_set_value", "Replace native control text under active task authorization, then read actual value for exact verification and return updated state. Use after.value/state directly; no separate snapshot/read is needed when the complete expected value is verified. value is plain Unicode text; do not pre-escape newlines, quotes or backslashes. For required terminal LF use value without final LF plus trailingNewlines=1 (or the exact count), avoiding provider trimming. verified means readback equals this call's input; also compare against the user's requested text. Prefer inputMethod=keyboard for user text entry in focusable text controls, including forms and system file dialogs; native value readback alone does not prove application change handlers ran. Explicit inputMethod=keyboard: activates the original window, focuses and verifies this exact text control, replaces all text using Unicode keyboard events and verifies actual readback. Keyboard input is limited to 8KB and requires a focusable text role; it needs no screenshot/frameId. Default inputMethod=value uses native AX/UIA value setting and never switches methods automatically. Password controls require user takeover.", schema(json!({"ref":{"type":"string"},"value":{"type":"string","maxLength":64000,"description":"Exact plain text including real newlines; JSON encoding is handled by the tool call format. No extra escaping or unescaping is performed."},"inputMethod":{"type":"string","enum":["value","keyboard"],"default":"value"},"trailingNewlines":{"type":"integer","minimum":0,"maximum":16,"description":"Explicit terminal LF count; overrides only terminal LF characters. Use with value without final LF to preserve a required ending across provider serialization. Omit to preserve value exactly."}}), &["ref","value"])),
        tool("computer_read", "Read provider-exposed text in Unicode scalar segments. Output is byte-bounded: continue with returned nextOffset while truncated, even if less than limit was returned. Does not claim unloaded/virtual history is complete.", schema(json!({"ref":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":2000000},"limit":{"type":"integer","minimum":1,"maximum":64000}}), &["ref"])),
        tool("computer_wait_for", "Poll a window for an exact selector and state: appears, disappears, enabled, or text_equals. Ambiguity is an error. Releases desktop between polls; cancellable by pause. Timeout never means success.", schema(json!({"windowRef":{"type":"string"},"role":{"type":"string"},"name":{"type":"string"},"identifier":{"type":"string"},"state":{"enum":["appears","disappears","enabled","text_equals"]},"text":{"type":"string"},"timeoutMs":{"type":"integer","minimum":100,"maximum":30000}}), &["windowRef","state"])),
    ]
}

async fn accessibility_tool(
    manager: &Arc<AutomationManager>,
    session_id: &str,
    session: &AutomationSession,
    name: &str,
    args: &Value,
    token: &CancellationToken,
) -> Result<Value, String> {
    let controller = {
        let mut worker = session.accessibility.lock().unwrap();
        if worker.as_ref().is_some_and(|w| !w.is_alive()) {
            worker.take();
        }
        worker
            .get_or_insert_with(|| Arc::new(accessibility::Controller::new()))
            .clone()
    };
    let mut arguments = args.clone();
    // No model-controlled receipt/proof can enter the internal approval path.
    if arguments.get("_proof").is_some() {
        return Err("invalid_argument: reserved field".into());
    }
    if name == "computer_key_window" {
        accessibility::window_key_input(args)?;
    }
    if name == "computer_set_value" {
        let (raw_value, _) = accessibility::text_input(args)?;
        let value =
            artifact::with_trailing_newlines(raw_value, optional_u64(args, "trailingNewlines")?)?;
        arguments["value"] = json!(value);
    }
    if name == "computer_open_app" {
        let path = std::path::Path::new(required_str(args, "appPath")?);
        if !path.is_absolute() {
            return Err("invalid_argument: appPath must be absolute".into());
        }
        let path = path
            .canonicalize()
            .map_err(|e| format!("application_not_found: {e}"))?;
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let supported =
            (cfg!(target_os = "macos") && extension.eq_ignore_ascii_case("app") && path.is_dir())
                || (cfg!(target_os = "windows")
                    && extension.eq_ignore_ascii_case("exe")
                    && path.is_file());
        if !supported {
            return Err(
                "invalid_argument: expected an installed .app bundle or .exe; no script/arguments"
                    .into(),
            );
        }
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        manager
            .approve(
                session_id,
                name,
                "确认打开应用",
                "请核对将要打开的应用路径。",
                json!({"appPath":path}),
                token,
            )
            .await?;
        let lease = native_lease(session, token).await?;
        let current = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if current.modified().ok() != metadata.modified().ok() || current.len() != metadata.len() {
            return Err(
                "target_changed: application changed while awaiting approval/desktop queue".into(),
            );
        }
        let result = controller
            .call("_open_app", json!({"appPath":path}), token, lease)
            .await?;
        session.computer.lock().unwrap().invalidate_frame();
        return Ok(tool_json_result(result));
    }
    if requires_desktop_consent(name) {
        let prepared = {
            let lease = native_lease(session, token).await?;
            controller
                .call(
                    "_prepare",
                    json!({"ref":required_str(args,"ref")?}),
                    token,
                    lease,
                )
                .await?
        };
        let mut details = prepared["target"].clone();
        details["action"] = json!(name);
        if name == "computer_set_value" {
            details["inputMethod"] = json!(if accessibility::text_input(args)?.1 {
                "keyboard"
            } else {
                "value"
            });
            details["characters"] = json!(arguments
                .get("value")
                .and_then(Value::as_str)
                .ok_or("invalid_argument: value must be a string")?
                .chars()
                .count());
        }
        if name == "computer_invoke" {
            details["nativeAction"] = json!(required_str(args, "action")?);
        }
        if name == "computer_key_window" {
            let (key, modifiers) = accessibility::window_key_input(args)?;
            details["key"] = json!(key);
            details["modifiers"] = json!(modifiers);
        }
        manager
            .approve(
                session_id,
                name,
                "确认原生桌面操作",
                "请核对目标控件；批准后仍会重新验证目标。输入原文不会显示在确认卡片中。",
                details,
                token,
            )
            .await?;
        arguments["_proof"] = prepared["proof"].clone();
    }
    if name == "computer_wait_for" {
        let state = required_str(args, "state")?;
        if !["appears", "disappears", "enabled", "text_equals"].contains(&state) {
            return Err("invalid_argument: unsupported wait state".into());
        }
        if state == "text_equals" {
            args.get("text")
                .and_then(Value::as_str)
                .ok_or("invalid_argument: text must be a string")?;
        }
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(
                optional_u64(args, "timeoutMs")?
                    .unwrap_or(5000)
                    .clamp(100, 30000),
            );
        loop {
            let observed = {
                let lease = native_lease(session, token).await?;
                let mut observed = controller
                    .call("computer_find", arguments.clone(), token, lease)
                    .await?;
                if state == "text_equals" && observed["status"] == "unique" {
                    let read_lease = native_lease(session, token).await?;
                    let read = controller
                        .call(
                            "_read_matches",
                            json!({"ref":observed["candidates"][0]["ref"],"limit":64000,"text":args["text"]}),
                            token,
                            read_lease,
                        )
                        .await?;
                    observed["textMatches"] = read["matches"].clone();
                }
                // Wait results retain the condition and one bounded observation;
                // large candidate pages belong to explicit find/continue calls.
                observed["candidatePreviewTruncated"] =
                    json!(observed["totalItems"].as_u64().unwrap_or(0) > 1);
                observed["nextCursor"] = Value::Null;
                observed["continuation"] =
                    json!("call computer_find to inspect full bounded candidates");
                observed["candidates"] = json!(observed["candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .take(1)
                    .collect::<Vec<_>>());
                observed
            };
            if observed["status"] == "ambiguous" {
                return Ok(tool_json_result(
                    json!({"status":"ambiguous","verified":false,"observation":observed}),
                ));
            }
            let found = observed["status"] == "unique";
            let met = match state {
                "appears" => found,
                "disappears" => !found && observed["truncated"] == false,
                "enabled" => found && observed["candidates"][0]["enabled"] == true,
                "text_equals" => found && observed["textMatches"] == true,
                _ => false,
            };
            if met {
                return Ok(tool_json_result(
                    json!({"status":"verified","verified":true,"observation":observed}),
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(tool_json_result(
                    json!({"status":"timeout","verified":false,"observation":observed}),
                ));
            }
            tokio::select! {biased; _=token.cancelled()=>return Err("cancelled: condition wait interrupted".into()),_=tokio::time::sleep(Duration::from_millis(200))=>{}}
        }
    }
    let lease = native_lease(session, token).await?;
    let result = controller.call(name, arguments, token, lease).await?;
    if requires_desktop_consent(name) {
        session.computer.lock().unwrap().invalidate_frame();
    }
    Ok(tool_json_result(result))
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn object_schema(required: &[&str]) -> Value {
    schema(json!({}), required)
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

async fn tools_call(
    manager: &Arc<AutomationManager>,
    session_id: &str,
    params: &Value,
) -> Result<Value, String> {
    let object = params
        .as_object()
        .ok_or_else(|| "tools/call params 必须是对象".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "tools/call 缺少字符串 name".to_string())?;
    let args = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err("工具 arguments 必须是对象".into());
    }
    if serde_json::to_vec(&args)
        .map_err(|error| format!("无法校验工具参数：{error}"))?
        .len()
        > MAX_TOOL_ARGUMENT_BYTES
    {
        return Err("工具参数超过 128KB 上限".into());
    }
    let result = match name {
        "automation_status" => serde_json::to_value(manager.status(session_id).await?)
            .map_err(|error| error.to_string())?,
        "automation_pause" => {
            manager
                .pause_with_reason(session_id, true, "agent_requested_pause")
                .await?;
            serde_json::to_value(manager.status(session_id).await?)
                .map_err(|error| error.to_string())?
        }
        "automation_stop" => {
            manager.stop(session_id).await?;
            serde_json::to_value(manager.status(session_id).await?)
                .map_err(|error| error.to_string())?
        }
        name if name.starts_with("browser_") => {
            return browser_tool(manager, session_id, name, &args).await;
        }
        name if name.starts_with("computer_") => {
            let result = computer_tool(manager, session_id, name, &args).await;
            manager.emit_status(session_id).await;
            return result;
        }
        other => return Err(format!("unknown tool: {other}")),
    };
    Ok(tool_json_result(result))
}

async fn browser_tool(
    manager: &Arc<AutomationManager>,
    session_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let session = manager
        .ensure_active(session_id, AutomationMode::BrowserUse)
        .await?;
    let action_token = session.action_token();
    if name == "browser_stop" {
        let _action_guard =
            enter_action(&session, &action_token, AutomationMode::BrowserUse).await?;
        if let Some(mut browser) = session.browser.lock().await.take() {
            browser.stop().await?;
        }
        manager.emit_status(session_id).await;
        return Ok(tool_json_result(json!({ "stopped": true })));
    }
    let (approval_plan, upload_paths) = browser_preflight(&session, &action_token, async {
    let launched = {
        let mut guard = session.browser.lock().await;
        let needs_launch = match guard.as_mut() {
            None => true,
            Some(browser) => match browser.status().await {
                Ok(status) => !status.running,
                Err(error) => {
                    tracing::warn!(%error, %session_id, "controlled browser connection was lost; restarting");
                    true
                }
            },
        };
        if needs_launch {
            if let Some(mut browser) = guard.take() {
                let _ = browser.stop().await;
            }
            let allow_private = *session.allow_private_network.lock().unwrap();
            *guard = Some(BrowserController::launch(session_id, allow_private).await?);
            true
        } else {
            false
        }
    };
    if launched {
        manager.emit_status(session_id).await;
    }

    // Never hold the browser mutex while waiting for a human decision. This
    // keeps status/pause/stop responsive throughout the approval window.
    let upload_paths = if name == "browser_upload" {
        validate_upload_paths(manager, session_id, args)?
    } else {
        Vec::new()
    };
    let approval_plan = if matches!(
        name,
        "browser_click"
            | "browser_type"
            | "browser_select"
            | "browser_upload"
            | "browser_key"
            | "browser_drag"
    ) {
        let reference = match name {
            "browser_type" | "browser_select" | "browser_upload" => {
                Some(required_str(args, "elementRef")?)
            }
            "browser_click" => optional_str(args, "elementRef")?,
            _ => None,
        };
        let interaction_input = match name {
            "browser_click" => {
                let (x, y) = if reference.is_none() {
                    (Some(required_f64(args, "x")?), Some(required_f64(args, "y")?))
                } else {
                    (None, None)
                };
                Some(json!({ "kind": "click", "reference": reference, "x": x, "y": y,
                    "button": optional_str(args, "button")?.unwrap_or("left") }))
            }
            "browser_key" => Some(json!({ "kind": "key", "key": required_str(args, "key")? })),
            "browser_drag" => Some(json!({
                "kind": "drag",
                "fromX": required_f64(args, "fromX")?,
                "fromY": required_f64(args, "fromY")?,
                "toX": required_f64(args, "toX")?,
                "toY": required_f64(args, "toY")?,
            })),
            _ => None,
        };
        let mut guard = session.browser.lock().await;
        let browser = guard.as_mut().ok_or("浏览器已停止，请重新开始操作网页")?;
        let (url, page_title, target) = browser_approval_context(browser, reference).await?;
        let interaction = if let Some(input) = interaction_input {
            let interaction = browser.prepare_interaction(input).await?;
            if interaction.url() != url {
                return Err("网页已变化，请重新观察后再请求确认".into());
            }
            Some(interaction)
        } else {
            None
        };
        let preview_target = interaction.as_ref().map(browser::BrowserInteraction::target);
        let destination = interaction.as_ref().map(browser::BrowserInteraction::destination);
        let form_submit_url = interaction
            .as_ref()
            .and_then(browser::BrowserInteraction::form_submit_url);
        let submits_form = form_submit_url.is_some();
        let common = json!({
            "url": url,
            "pageTitle": page_title,
            "target": preview_target.as_ref().or(target.as_ref()),
            "destination": destination,
            "formSubmitUrl": form_submit_url,
        });
        let (title, description, details) = match name {
            "browser_click" => {
                let high_risk = preview_target.as_ref().or(target.as_ref()).is_some_and(browser_target_looks_risky);
                let title = if submits_form {
                    "确认提交网页表单"
                } else if high_risk {
                    "确认网页上的重要点击"
                } else {
                    "确认网页点击"
                };
                let description = if submits_form {
                    "表单内容可能发送到下方地址。请核对提交地址和目标控件。"
                } else if high_risk {
                    "该控件可能发送、购买、删除或确认重要操作。请核对网站和目标后继续。"
                } else {
                    "代理即将点击当前网页。请核对网站和目标后继续。"
                };
                (
                    title,
                    description,
                    merge_json(
                        common,
                        json!({
                            "x": optional_f64(args, "x")?,
                            "y": optional_f64(args, "y")?,
                            "button": optional_str(args, "button")?.unwrap_or("left"),
                            "clickCount": optional_u64(args, "clickCount")?.unwrap_or(1),
                        }),
                    ),
                )
            }
            "browser_type" => {
                let password = target
                    .as_ref()
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str)
                    == Some("password");
                if password {
                    return Err("为保护凭据，代理不能接收或填写密码框。请暂停自动化，请用户接管受控浏览器完成填写，再由用户点击“继续”。".into());
                }
                (
                    "确认向网页填写文本",
                    "文本可能在输入时立即发送给网站。为保护隐私，确认卡片不显示具体内容。",
                    merge_json(
                        common,
                        json!({
                            "characters": required_str(args, "text")?.chars().count(),
                            "passwordField": password,
                            "replace": optional_bool(args, "replace")?.unwrap_or(true),
                        }),
                    ),
                )
            }
            "browser_select" => (
                "确认更改网页选项",
                "更改选项可能立即触发网站操作。请核对目标后继续。",
                merge_json(
                    common,
                    json!({ "selectionCount": required_string_array(args, "values", 100)?.len() }),
                ),
            ),
            "browser_upload" => {
                let names = upload_paths
                    .iter()
                    .filter_map(|path| std::path::Path::new(path).file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                (
                    "确认向网站上传文件",
                    "文件内容将离开本机并提交给当前网站。仅允许上传当前任务工作区内的普通文件。",
                    merge_json(
                        common,
                        json!({ "files": names, "fileCount": upload_paths.len() }),
                    ),
                )
            }
            "browser_key" => (
                if submits_form { "确认按键提交表单" } else { "确认网页按键操作" },
                if submits_form { "表单内容可能发送到下方地址。请核对提交地址和当前焦点。" }
                    else { "按键或快捷键可能提交内容、执行命令或改变网站状态。" },
                merge_json(
                    common,
                    json!({
                        "key": required_str(args, "key")?,
                        "modifiers": optional_string_array(args, "modifiers", 4)?,
                    }),
                ),
            ),
            "browser_drag" => (
                "确认网页拖动操作",
                "拖动可能更改顺序、位置或触发网站操作。",
                merge_json(
                    common,
                    json!({
                        "fromX": required_f64(args, "fromX")?,
                        "fromY": required_f64(args, "fromY")?,
                        "toX": required_f64(args, "toX")?,
                        "toY": required_f64(args, "toY")?,
                    }),
                ),
            ),
            _ => unreachable!(),
        };
        Some(BrowserApprovalPlan {
            title: title.to_string(),
            description: description.to_string(),
            details,
            expected_url: url,
            expected_target: reference.map(str::to_string).zip(target),
            interaction,
        })
    } else {
        None
    };
    Ok((approval_plan, upload_paths))
    }).await?;

    if let Some(plan) = approval_plan.as_ref() {
        manager
            .approve(
                session_id,
                name,
                &plan.title,
                &plan.description,
                plan.details.clone(),
                &action_token,
            )
            .await?;
    }

    let _action_guard = enter_action(&session, &action_token, AutomationMode::BrowserUse).await?;
    let operation = async {
        let mut guard = session.browser.lock().await;
        let browser = guard.as_mut().ok_or("浏览器已停止，请重新开始操作网页")?;
        browser.enforce_current_url_policy().await?;
        if let Some(plan) = approval_plan.as_ref() {
            if browser.current_url().await? != plan.expected_url {
                return Err("等待确认期间网页已变化，为避免误操作已取消，请重新检查页面".into());
            }
            if let Some((reference, expected)) = &plan.expected_target {
                let actual = browser_element_signature(browser, reference).await?;
                if actual != *expected {
                    return Err("等待确认期间目标控件已变化，操作已取消".into());
                }
            }
        }
        let interaction = || {
            approval_plan
                .as_ref()
                .and_then(|plan| plan.interaction.as_ref())
                .ok_or("网页操作缺少已确认的目标")
        };
        let result = match name {
            "browser_start" => {
                serde_json::to_value(browser.status().await?).map_err(|error| error.to_string())?
            }
            "browser_navigate" => browser.navigate(required_str(args, "url")?).await?,
            "browser_snapshot" => browser.snapshot().await?,
            "browser_screenshot" => {
                let (data, meta) = browser.screenshot().await?;
                drop(guard);
                manager.emit_status(session_id).await;
                return Ok(tool_image_result(meta, "image/png", data));
            }
            "browser_click" => {
                browser
                    .click(
                        interaction()?,
                        optional_str(args, "button")?.unwrap_or("left"),
                        optional_u64(args, "clickCount")?.unwrap_or(1) as u32,
                    )
                    .await?
            }
            "browser_hover" => browser.hover(required_str(args, "elementRef")?).await?,
            "browser_type" => {
                let reference = required_str(args, "elementRef")?;
                browser
                    .type_text(
                        reference,
                        required_str(args, "text")?,
                        optional_bool(args, "replace")?.unwrap_or(true),
                    )
                    .await?
            }
            "browser_select" => {
                let values = required_string_array(args, "values", 100)?;
                browser
                    .select(required_str(args, "elementRef")?, &values)
                    .await?
            }
            "browser_upload" => {
                browser
                    .upload(required_str(args, "elementRef")?, &upload_paths)
                    .await?
            }
            "browser_key" => {
                let key = required_str(args, "key")?;
                browser
                    .key(
                        interaction()?,
                        key,
                        &optional_string_array(args, "modifiers", 4)?,
                    )
                    .await?
            }
            "browser_scroll" => {
                browser
                    .scroll(
                        optional_f64(args, "deltaX")?.unwrap_or(0.0),
                        required_f64(args, "deltaY")?,
                    )
                    .await?
            }
            "browser_drag" => {
                browser
                    .drag(
                        interaction()?,
                        required_f64(args, "fromX")?,
                        required_f64(args, "fromY")?,
                        required_f64(args, "toX")?,
                        required_f64(args, "toY")?,
                    )
                    .await?
            }
            "browser_tabs" => match optional_str(args, "action")?.unwrap_or("list") {
                "list" => serde_json::to_value(browser.tabs().await?)
                    .map_err(|error| error.to_string())?,
                "new" => serde_json::to_value(browser.new_tab(optional_str(args, "url")?).await?)
                    .map_err(|error| error.to_string())?,
                "select" => {
                    serde_json::to_value(browser.select_tab(required_str(args, "targetId")?).await?)
                        .map_err(|error| error.to_string())?
                }
                "close" => browser.close_tab(required_str(args, "targetId")?).await?,
                _ => return Err("tabs action 必须是 list、new、select 或 close".into()),
            },
            "browser_wait" => {
                browser
                    .wait(optional_u64(args, "milliseconds")?.unwrap_or(1_000))
                    .await?
            }
            "browser_downloads" => {
                serde_json::to_value(browser.downloads()?).map_err(|error| error.to_string())?
            }
            _ => return Err(format!("unknown browser tool: {name}")),
        };
        browser.enforce_current_url_policy().await?;
        drop(guard);
        manager.emit_status(session_id).await;
        Ok(tool_json_result(result))
    };
    if matches!(name, "browser_click" | "browser_key" | "browser_drag") {
        // These commands have paired press/release events. Once admitted,
        // finish the short sequence so a pause cannot strand a pressed mouse
        // button or keyboard key. pause() still waits on the execution gate,
        // therefore no action remains in flight when takeover returns.
        operation.await
    } else {
        tokio::select! {
            biased;
            _ = action_token.cancelled() => Err("自动化已暂停，正在执行的浏览器操作已取消".into()),
            result = operation => result,
        }
    }
}

async fn browser_approval_context(
    browser: &mut BrowserController,
    reference: Option<&str>,
) -> Result<(String, String, Option<Value>), String> {
    let snapshot = browser.snapshot().await?;
    let url = snapshot
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "浏览器快照缺少当前网址".to_string())?
        .to_string();
    let title = snapshot
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let target = reference
        .map(|reference| element_signature_from_snapshot(&snapshot, reference))
        .transpose()?;
    Ok((url, title, target))
}

async fn browser_element_signature(
    browser: &mut BrowserController,
    reference: &str,
) -> Result<Value, String> {
    let snapshot = browser.snapshot().await?;
    element_signature_from_snapshot(&snapshot, reference)
}

fn element_signature_from_snapshot(snapshot: &Value, reference: &str) -> Result<Value, String> {
    let element = snapshot
        .get("elements")
        .and_then(Value::as_array)
        .and_then(|elements| {
            elements
                .iter()
                .find(|element| element.get("ref").and_then(Value::as_str) == Some(reference))
        })
        .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
    // Deliberately exclude bounds, current value and checked state. Layout and
    // transient state may change while a user reads the card, but the semantic
    // identity of the approved target must remain stable. Password/value data
    // can therefore never enter an approval event either.
    Ok(json!({
        "ref": reference,
        "tag": element.get("tag").and_then(Value::as_str).unwrap_or_default(),
        "role": element.get("role").and_then(Value::as_str).unwrap_or_default(),
        "name": element.get("name").and_then(Value::as_str).unwrap_or_default(),
        "type": element.get("type").and_then(Value::as_str).unwrap_or_default(),
        "href": element.get("href").and_then(Value::as_str),
        "disabled": element.get("disabled").and_then(Value::as_bool).unwrap_or(false),
    }))
}

fn browser_target_looks_risky(element: &Value) -> bool {
    let searchable = ["name", "href", "type"]
        .into_iter()
        .filter_map(|key| element.get(key).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    const RISKY: [&str; 24] = [
        "pay", "purchase", "buy", "checkout", "order", "submit", "send", "publish", "post",
        "delete", "remove", "confirm", "accept", "agree", "sign in", "log in", "付款", "购买",
        "下单", "发送", "发布", "删除", "确认", "同意",
    ];
    RISKY.iter().any(|keyword| searchable.contains(keyword))
}

fn merge_json(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    base
}

struct ComputerApprovalPlan {
    title: &'static str,
    description: &'static str,
    details: Value,
}

fn computer_approval_plan(
    session: &AutomationSession,
    name: &str,
    args: &Value,
) -> Result<ComputerApprovalPlan, String> {
    let frame_id = required_str(args, "frameId")?;
    let frame = session.computer.lock().unwrap().frame_meta(frame_id)?;
    let frame_details = json!({
        "frameId": frame_id,
        "displayId": frame.display_id,
        "targetName": frame.target_name,
    });
    match name {
        "computer_click" => Ok(ComputerApprovalPlan {
            title: "确认电脑点击",
            description: "点击可能提交、发送、购买、删除或确认操作。请核对截图位置后继续。",
            details: merge_json(
                frame_details,
                json!({
                    "x": required_f64(args, "x")?,
                    "y": required_f64(args, "y")?,
                    "button": optional_str(args, "button")?.unwrap_or("left"),
                    "clickCount": optional_u64(args, "clickCount")?.unwrap_or(1),
                }),
            ),
        }),
        "computer_drag" => Ok(ComputerApprovalPlan {
            title: "确认电脑拖动操作",
            description: "拖动可能移动文件、改变顺序或触发应用操作。",
            details: merge_json(
                frame_details,
                json!({
                    "fromX": required_f64(args, "fromX")?,
                    "fromY": required_f64(args, "fromY")?,
                    "toX": required_f64(args, "toX")?,
                    "toY": required_f64(args, "toY")?,
                    "durationMs": optional_u64(args, "durationMs")?.unwrap_or(500),
                }),
            ),
        }),
        "computer_type" => Ok(ComputerApprovalPlan {
            title: "确认在电脑中输入文本",
            description:
                "文本可能被当前应用立即读取、发送或提交。为保护隐私，确认卡片不显示具体内容。",
            details: merge_json(
                frame_details,
                json!({
                    "characters": required_str(args, "text")?.chars().count(),
                }),
            ),
        }),
        "computer_key" => Ok(ComputerApprovalPlan {
            title: "确认电脑按键操作",
            description: "按键或快捷键可能提交内容、执行命令或改变应用状态。",
            details: merge_json(
                frame_details,
                json!({
                    "key": required_str(args, "key")?,
                    "modifiers": optional_string_array(args, "modifiers", 4)?,
                }),
            ),
        }),
        _ => Err(format!("missing independent confirmation plan for {name}")),
    }
}

async fn computer_tool(
    manager: &Arc<AutomationManager>,
    session_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let normalized = accessibility::normalize_integer_options(name, args)?;
    let args = &normalized;
    let session = manager
        .ensure_active(session_id, AutomationMode::ComputerUse)
        .await?;
    if ComputerController::capability().desktop_available == Some(false) {
        manager
            .pause_with_reason(session_id, true, "desktop_unavailable")
            .await?;
        return Err("桌面暂不可用，任务已暂停；解锁电脑后发送继续可从现场续做".into());
    }
    let action_token = session.action_token();
    if name == "computer_click" && accessibility::native_click_ref(args)?.is_some() {
        return accessibility_tool(
            manager,
            session_id,
            &session,
            "computer_click_control",
            args,
            &action_token,
        )
        .await;
    }
    if name == "computer_verify_file" {
        let _read_guard =
            enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
        let scope = session.task.lock().unwrap().scope.clone();
        let count = optional_u64(args, "expectedTrailingNewlines")?;
        let expected = optional_str(args, "expectedText")?
            .map(|text| artifact::with_trailing_newlines(text, count))
            .transpose()?;
        if count.is_some() && expected.is_none() {
            return Err("invalid_argument: expectedTrailingNewlines requires expectedText".into());
        }
        let locations = artifact::Locations {
            home: dirs::home_dir(),
            desktop: dirs::desktop_dir(),
            documents: dirs::document_dir(),
            downloads: dirs::download_dir(),
            workspace: Some(
                manager
                    .app
                    .state::<crate::commands::AppState>()
                    .session_workspace(session_id)?,
            ),
        };
        let result = artifact::verify_file_at(
            scope.as_deref(),
            required_str(args, "path")?,
            expected.as_deref(),
            &locations,
        )?;
        return Ok(tool_json_result(result));
    }
    if accessibility_tools()
        .iter()
        .any(|tool| tool["name"] == name)
    {
        return accessibility_tool(manager, session_id, &session, name, args, &action_token).await;
    }
    if requires_desktop_consent(name) {
        let plan = computer_approval_plan(&session, name, args)?;
        manager
            .approve(
                session_id,
                name,
                plan.title,
                plan.description,
                plan.details,
                &action_token,
            )
            .await?;
        manager
            .ensure_active(session_id, AutomationMode::ComputerUse)
            .await?;
    }
    let _desktop = if name == "computer_wait" {
        None
    } else {
        Some(desktop_coordinator::acquire(&action_token).await?)
    };
    let result = match name {
        "computer_capabilities" => serde_json::to_value(ComputerController::capability())
            .map_err(|error| error.to_string())?,
        "computer_displays" => serde_json::to_value(ComputerController::displays()?)
            .map_err(|error| error.to_string())?,
        "computer_screenshot" => {
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            let frame = session
                .computer
                .lock()
                .unwrap()
                .screenshot(optional_str(args, "displayId")?)?;
            let meta = serde_json::to_value(&frame.meta).map_err(|error| error.to_string())?;
            return Ok(tool_image_result(meta, "image/png", frame.png_base64));
        }
        "computer_move" => {
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            let (x, y) = session.computer.lock().unwrap().move_pointer(
                required_str(args, "frameId")?,
                required_f64(args, "x")?,
                required_f64(args, "y")?,
            )?;
            json!({ "moved": true, "desktopX": x, "desktopY": y, "frameInvalidated": true })
        }
        // 点击必须经过上方桌面授权门；常规权限模式不能代替用户桌面授权。
        "computer_click" => {
            let frame_id = required_str(args, "frameId")?;
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            let (x, y) = session.computer.lock().unwrap().click(
                frame_id,
                required_f64(args, "x")?,
                required_f64(args, "y")?,
                optional_str(args, "button")?.unwrap_or("left"),
                optional_u64(args, "clickCount")?.unwrap_or(1) as u32,
            )?;
            json!({ "clicked": true, "desktopX": x, "desktopY": y, "frameInvalidated": true })
        }
        // 拖动沿用同一桌面授权门和派发前的画面验证。
        "computer_drag" => {
            let frame_id = required_str(args, "frameId")?;
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            session.computer.lock().unwrap().drag(
                frame_id,
                required_f64(args, "fromX")?,
                required_f64(args, "fromY")?,
                required_f64(args, "toX")?,
                required_f64(args, "toY")?,
                optional_u64(args, "durationMs")?.unwrap_or(500),
            )?;
            json!({ "dragged": true, "frameInvalidated": true })
        }
        "computer_scroll" => {
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            let delta_x = optional_i64(args, "deltaX")?
                .unwrap_or(0)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            let delta_y = required_i64(args, "deltaY")?
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            session.computer.lock().unwrap().scroll(
                required_str(args, "frameId")?,
                delta_x,
                delta_y,
            )?;
            json!({ "scrolled": true, "frameInvalidated": true })
        }
        // 文本输入沿用上方桌面授权门，不因常规工具的自动批准而跳过。
        // 详见 DESKTOP_WRITE_TOOLS。
        "computer_type" => {
            let frame_id = required_str(args, "frameId")?;
            let characters = required_str(args, "text")?.chars().count();
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            session
                .computer
                .lock()
                .unwrap()
                .type_text(frame_id, required_str(args, "text")?)?;
            json!({ "typed": true, "characters": characters, "frameInvalidated": true })
        }
        // 按键沿用上方桌面授权门及目标焦点验证。
        // 详见 DESKTOP_WRITE_TOOLS。
        "computer_key" => {
            let frame_id = required_str(args, "frameId")?;
            let key = required_str(args, "key")?;
            let modifiers = optional_string_array(args, "modifiers", 4)?;
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            session
                .computer
                .lock()
                .unwrap()
                .key(frame_id, key, &modifiers)?;
            json!({ "pressed": key, "modifiers": modifiers, "frameInvalidated": true })
        }
        "computer_wait" => {
            let milliseconds = optional_u64(args, "milliseconds")?
                .unwrap_or(1_000)
                .clamp(50, 30_000);
            tokio::select! {
                _ = action_token.cancelled() => {
                    return Err("自动化已暂停，等待操作已取消".into());
                }
                _ = tokio::time::sleep(Duration::from_millis(milliseconds)) => {}
            }
            let _action_guard =
                enter_action(&session, &action_token, AutomationMode::ComputerUse).await?;
            session.computer.lock().unwrap().invalidate_frame();
            json!({ "waitedMs": milliseconds })
        }
        _ => return Err(format!("unknown computer tool: {name}")),
    };
    Ok(tool_json_result(result))
}

fn tool_json_result(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": false,
    })
}

fn tool_image_result(meta: Value, mime_type: &str, data: String) -> Value {
    json!({
        "content": [
            { "type": "text", "text": meta.to_string() },
            { "type": "image", "mimeType": mime_type, "data": data }
        ],
        "isError": false,
    })
}

fn tool_error_result(message: &str) -> Value {
    let truncated = message.chars().count() > 8_192;
    let message = message.chars().take(8_192).collect::<String>();
    let message = if truncated {
        format!("{message}…")
    } else {
        message
    };
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn rpc_result(id: Value, result: Value) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    let text = body.to_string();
    if text.len() > MAX_RPC_RESPONSE_BYTES {
        return rpc_error(Value::Null, -32603, "response exceeds 24MB limit".into());
    }
    ([(header::CONTENT_TYPE, "application/json")], text).into_response()
}

fn rpc_error(id: Value, code: i64, message: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            .to_string(),
    )
        .into_response()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn validate_session_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.chars().count() > MAX_SESSION_ID_CHARS
        || value.chars().any(char::is_control)
    {
        Err("自动化会话 ID 无效".into())
    } else {
        Ok(())
    }
}

fn validate_upload_paths(
    manager: &AutomationManager,
    session_id: &str,
    args: &Value,
) -> Result<Vec<String>, String> {
    const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;

    let requested = required_string_array(args, "paths", 20)?;
    let state = manager.app.state::<crate::commands::AppState>();
    let workspace = state.session_workspace(session_id)?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("无法校验任务工作区：{error}"))?;
    let mut total_bytes = 0_u64;
    let mut canonical_paths = Vec::with_capacity(requested.len());
    for requested_path in requested {
        if requested_path.chars().any(char::is_control) {
            return Err("上传文件路径无效".into());
        }
        let raw = std::path::PathBuf::from(&requested_path);
        let candidate = if raw.is_absolute() {
            raw
        } else {
            workspace.join(raw)
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("无法读取待上传文件 {} ：{error}", candidate.display()))?;
        if !canonical.starts_with(&workspace) {
            return Err("为防止本地数据泄露，操作网页时只允许上传当前任务工作区内的文件".into());
        }
        let metadata = std::fs::metadata(&canonical)
            .map_err(|error| format!("无法检查待上传文件：{error}"))?;
        if !metadata.is_file() {
            return Err(format!("只能上传普通文件：{}", canonical.display()));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!(
                "单个上传文件不能超过 100MB：{}",
                canonical.display()
            ));
        }
        total_bytes = total_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "上传文件总大小溢出".to_string())?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err("单次上传文件总大小不能超过 500MB".into());
        }
        canonical_paths.push(canonical.to_string_lossy().into_owned());
    }
    Ok(canonical_paths)
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少字符串参数 {key}"))
}

fn optional_str<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(format!("参数 {key} 必须是字符串")),
    }
}

fn required_f64(value: &Value, key: &str) -> Result<f64, String> {
    optional_f64(value, key)?.ok_or_else(|| format!("缺少数字参数 {key}"))
}

fn optional_f64(value: &Value, key: &str) -> Result<Option<f64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => arguments::number(value)
            .and_then(|n| n.as_f64())
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| format!("参数 {key} 必须是有限数字")),
    }
}

fn required_i64(value: &Value, key: &str) -> Result<i64, String> {
    optional_i64(value, key)?.ok_or_else(|| format!("缺少整数参数 {key}"))
}

fn optional_i64(value: &Value, key: &str) -> Result<Option<i64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => arguments::number(value)
            .and_then(|n| n.as_i64())
            .map(Some)
            .ok_or_else(|| format!("参数 {key} 必须是整数")),
    }
}

fn optional_u64(value: &Value, key: &str) -> Result<Option<u64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => arguments::number(value)
            .and_then(|n| n.as_u64())
            .map(Some)
            .ok_or_else(|| format!("参数 {key} 必须是非负整数")),
    }
}

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("参数 {key} 必须是布尔值")),
    }
}

fn required_string_array(value: &Value, key: &str, max: usize) -> Result<Vec<String>, String> {
    let values = optional_string_array(value, key, max)?;
    if values.is_empty() {
        Err(format!("参数 {key} 不能为空"))
    } else {
        Ok(values)
    }
}

fn optional_string_array(value: &Value, key: &str, max: usize) -> Result<Vec<String>, String> {
    let Some(raw) = value.get(key) else {
        return Ok(Vec::new());
    };
    let values = raw
        .as_array()
        .ok_or_else(|| format!("参数 {key} 必须是字符串数组"))?;
    if values.len() > max {
        return Err(format!("参数 {key} 数量超过 {max}"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("参数 {key} 必须是字符串数组"))
        })
        .collect()
}

/// Persist the rotating loopback endpoint after the first live session exists.
pub fn persist_registration(tx: &echo_agent_acp::AcpAgentTx, session_id: &str) {
    {
        let mut done = PERSISTED.lock().unwrap();
        if *done {
            return;
        }
        *done = true;
    }
    let (Some(url), Some(authorization)) = (server_url(), authorization_header()) else {
        *PERSISTED.lock().unwrap() = false;
        return;
    };
    let tx = tx.clone();
    let session_id = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let mut headers = serde_json::Map::new();
        headers.insert(AUTH_HEADER.into(), Value::String(authorization));
        headers.insert(SESSION_HEADER.into(), Value::String("${session_id}".into()));
        let payload = json!({
            "session_id": session_id,
            "server_name": MCP_SERVER_NAME,
            "url": url,
            "headers": headers,
            "enabled": true,
        });
        if let Err(error) = crate::ext::call_ext_value(
            &tx,
            "echo.agent/mcp/upsert",
            crate::ext::raw_params(&payload),
        )
        .await
        {
            tracing::warn!(?error, "automation MCP registration failed");
            *PERSISTED.lock().unwrap() = false;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_mode_preserves_pause_and_private_network_revocation() {
        for mode in [AutomationMode::BrowserUse, AutomationMode::ComputerUse] {
            let session = AutomationSession::new(mode);
            session.commit_mode(mode, false);
            assert!(session.task.lock().unwrap().paused());
            assert!(!*session.allow_private_network.lock().unwrap());
        }
    }

    #[test]
    fn restored_mode_does_not_interrupt_active_task_or_activate_changed_mode() {
        let session = AutomationSession::new(AutomationMode::Default);
        session.commit_mode(AutomationMode::BrowserUse, true);
        session.commit_mode(AutomationMode::BrowserUse, false);
        assert!(!session.task.lock().unwrap().paused());
        session.commit_mode(AutomationMode::ComputerUse, false);
        assert!(session.task.lock().unwrap().paused());
    }

    #[test]
    fn explicit_mode_selection_can_activate_after_restored_pause() {
        let session = AutomationSession::new(AutomationMode::BrowserUse);
        session.commit_mode(AutomationMode::BrowserUse, false);
        assert!(session.task.lock().unwrap().paused());
        session.commit_mode(AutomationMode::BrowserUse, true);
        assert!(!session.task.lock().unwrap().paused());
    }

    #[test]
    fn computer_task_consent_is_local_and_revoked_at_control_boundaries() {
        let session = AutomationSession::new(AutomationMode::ComputerUse);
        assert!(session
            .authorize_computer_task(&session.action_token())
            .is_err());
        session.commit_mode(AutomationMode::ComputerUse, true);
        let token = session.action_token();
        assert!(!session.computer_task_authorized(&token));
        session.authorize_computer_task(&token).unwrap();
        assert!(session.computer_task_authorized(&token));
        let other = AutomationSession::new(AutomationMode::ComputerUse);
        other.commit_mode(AutomationMode::ComputerUse, true);
        assert!(!other.computer_task_authorized(&other.action_token()));
        session.reset_action_token();
        assert!(token.is_cancelled());
        assert!(!session.computer_task_authorized(&session.action_token()));
        assert!(session.authorize_computer_task(&token).is_err());
        let token = session.action_token();
        session.authorize_computer_task(&token).unwrap();
        session.cancel_actions();
        assert!(!session.computer_task_authorized(&token));
        session.reset_action_token();
        assert!(!session.computer_task_authorized(&session.action_token()));
        assert!(session.authorize_computer_task(&token).is_err());
        session.commit_mode(AutomationMode::BrowserUse, true);
        assert!(session
            .authorize_computer_task(&session.action_token())
            .is_err());
    }

    #[test]
    fn listener_registration_from_sync_context_uses_tauri_runtime() {
        let listener = bind_loopback().expect("bind loopback listener");
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);

        tauri::async_runtime::spawn(async move {
            let result = to_tokio_listener(listener)
                .await
                .and_then(|listener| listener.local_addr());
            let _ = result_tx.send(result);
        });

        let address = result_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("Tauri runtime should poll listener registration")
            .expect("listener registration should succeed");
        assert!(address.ip().is_loopback());
    }

    #[test]
    fn modes_are_explicit_and_closed() {
        assert_eq!(
            AutomationMode::parse("browser").unwrap(),
            AutomationMode::BrowserUse
        );
        assert_eq!(
            AutomationMode::parse("computer_use").unwrap(),
            AutomationMode::ComputerUse
        );
        assert!(AutomationMode::parse("unsafe").is_err());
    }

    #[test]
    fn tool_schema_disallows_unknown_fields() {
        let listed = tools_list_result();
        let tools = listed["tools"].as_array().unwrap();
        assert!(tools.len() >= 20);
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
        assert!(!tools.iter().any(|tool| tool["name"] == "automation_resume"));
        let click = tools
            .iter()
            .find(|tool| tool["name"] == "computer_click")
            .unwrap();
        assert_eq!(
            click["inputSchema"]["oneOf"],
            json!([
                {"required":["ref"]}, {"required":["frameId","x","y"]}
            ])
        );
        for tool_name in ["computer_click", "computer_type", "computer_key"] {
            let tool = tools.iter().find(|tool| tool["name"] == tool_name).unwrap();
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            assert!(!properties.contains_key("consequential"));
            assert!(!properties.contains_key("sensitive"));
            assert!(!properties.contains_key("intent"));
        }
    }

    #[test]
    fn session_ids_reject_control_characters() {
        assert!(validate_session_id("session-1").is_ok());
        assert!(validate_session_id("bad\nvalue").is_err());
    }

    #[tokio::test]
    async fn takeover_cancels_browser_preparation_and_releases_execution_gate() {
        let session = AutomationSession::new(AutomationMode::BrowserUse);
        session.task.lock().unwrap().resume(false);
        let token = session.action_token();
        let preparation = browser_preflight(
            &session,
            &token,
            std::future::pending::<Result<(), String>>(),
        );
        let cancel = async {
            tokio::task::yield_now().await;
            session.cancel_actions();
            session.task.lock().unwrap().pause("user_paused");
            let _exclusive = session.execution_gate.write().await;
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(preparation, cancel)
        })
        .await
        .expect("takeover must not wait for browser startup or a CDP timeout");
        assert!(result.unwrap_err().contains("准备操作已取消"));
        assert!(enter_action(&session, &token, AutomationMode::BrowserUse)
            .await
            .is_err());
    }

    #[test]
    fn requires_desktop_consent_matches_const_list() {
        for tool in super::DESKTOP_WRITE_TOOLS {
            assert!(
                super::requires_desktop_consent(tool),
                "expected always-confirm for {tool}"
            );
        }
        for tool in [
            "computer_capabilities",
            "computer_displays",
            "computer_screenshot",
            "computer_move",
            "computer_scroll",
            "computer_wait",
            "browser_click",
            "browser_navigate",
        ] {
            assert!(
                !super::requires_desktop_consent(tool),
                "expected NOT always-confirm for {tool}"
            );
        }
    }
    #[test]
    fn native_desktop_tools_are_listed_and_writes_cannot_opt_out_of_approval() {
        let list = tools_list_result();
        let tools = list["tools"].as_array().unwrap();
        for name in [
            "computer_list_apps",
            "computer_list_windows",
            "computer_activate_window",
            "computer_snapshot",
            "computer_find",
            "computer_invoke",
            "computer_set_value",
            "computer_read",
            "computer_wait_for",
            "computer_open_app",
            "computer_key_window",
            "computer_file_dialog",
        ] {
            let tool = tools
                .iter()
                .find(|t| t["name"] == name)
                .expect("native tool discoverable by Runtime");
            for bypass in [
                "_proof",
                "safe",
                "consequential",
                "skipApproval",
                "approved",
            ] {
                assert!(tool["inputSchema"]["properties"].get(bypass).is_none());
            }
        }
        for name in [
            "computer_activate_window",
            "computer_invoke",
            "computer_set_value",
            "computer_open_app",
            "computer_key_window",
            "computer_file_dialog",
        ] {
            assert!(requires_desktop_consent(name));
        }
    }

    #[tokio::test]
    async fn pause_boundary_waits_for_dispatched_native_event_and_rejects_later_input() {
        let session = AutomationSession::new(AutomationMode::ComputerUse);
        session.task.lock().unwrap().resume(false);
        let token = session.action_token();
        let (started, observed) = oneshot::channel();
        let executed = std::sync::atomic::AtomicUsize::new(0);
        let event = async {
            let _gate = enter_action(&session, &token, AutomationMode::ComputerUse)
                .await
                .unwrap();
            let _ = started.send(());
            // Represents an event already submitted to the OS; it cannot be withdrawn.
            tokio::time::sleep(Duration::from_millis(10)).await;
            executed.fetch_add(1, Ordering::SeqCst);
        };
        let pause = async {
            observed.await.unwrap();
            session.cancel_actions();
            session.task.lock().unwrap().pause("user_paused");
            let _exclusive = session.execution_gate.write().await;
            assert_eq!(executed.load(Ordering::SeqCst), 1);
        };
        tokio::join!(event, pause);
        assert!(enter_action(&session, &token, AutomationMode::ComputerUse)
            .await
            .is_err());
        assert_eq!(executed.load(Ordering::SeqCst), 1);
    }
}
