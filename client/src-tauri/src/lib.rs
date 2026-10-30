// EchoAgent Tauri backend library entry point.
//
// Spawns the in-process EchoAgent agent (agent_runtime::spawn_agent_runtime), wires the ACP stream
// to Tauri events (bridge::spawn_dispatcher), and registers the command
// table (commands) that the React frontend invokes.

mod agent_admin;
mod agent_config;
mod agent_runtime;
mod agents_store;
mod app_updater;
mod attachment_blob;
mod attachment_preview;
mod automation;
mod automations;
mod backup;
mod bridge;
mod coding;
mod coding_closed_documents;
mod coding_completion;
mod coding_document_bridge;
mod coding_external_tools;
mod coding_patch;
mod coding_plan;
mod coding_review_runtime;
mod coding_workspace;
mod commands;
#[cfg(test)]
#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-runtime/src/session/acp_session_impl/desktop_delivery.rs"]
mod desktop_delivery_tests;
mod desktop_preferences;
mod desktop_tray;
mod desktop_tray_model;
mod desktop_validation;
mod document_export;
mod expert_avatar;
mod ext;
mod groups;
mod host_watchdog;
mod knowledge_traces;
mod live_info_mcp;
mod logging;
mod mcp;
mod meeting_capture;
mod meeting_minutes;
mod meta;
mod native_browser_validation;
mod native_coding_validation;
mod native_core_validation;
mod native_exit_validation;
mod native_ide_validation;
mod native_review_model;
mod native_review_scope;
mod notifications;
mod office_mcp;
mod org;
mod org_mcp;
mod paths;
mod permission_config;
mod personal_knowledge;
mod policy;
mod process_identity;
mod process_supervisor;
mod projects;
mod provider_response;
mod providers;
mod recording_recovery;
mod recovery_policy;
mod resource_identity;
mod session_artifacts;
mod session_creation;
mod session_title;
mod session_workspace;
mod sessions;
mod shell_fs;
mod skill_installer;
mod skills;
mod storage;
mod team_mcp;
mod team_orchestration;
mod theia;
mod webview_recovery;
mod weixin;

use bridge::{FolderTrusts, Permissions, PlanApprovals, Questions};
use commands::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::Emitter;
use tauri::Manager;

static EXIT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static EXIT_READY: AtomicBool = AtomicBool::new(false);
static EXIT_LISTENER_READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn exit_review_lifecycle_snapshot() -> serde_json::Value {
    serde_json::json!({
        "listenerReady": EXIT_LISTENER_READY.load(Ordering::Acquire),
        "exitInProgress": EXIT_IN_PROGRESS.load(Ordering::Acquire),
        "exitReady": EXIT_READY.load(Ordering::Acquire),
    })
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitPreparationNotice {
    request_id: String,
    recording_saved: bool,
}
static ACTIVE_EXIT_REQUEST: std::sync::Mutex<Option<ExitPreparationNotice>> =
    std::sync::Mutex::new(None);
type ExitPreparationResult = Result<(), String>;
static EXIT_PREPARATION: std::sync::Mutex<
    Option<(String, tokio::sync::oneshot::Sender<ExitPreparationResult>)>,
> = std::sync::Mutex::new(None);
const SHUTDOWN_STEP_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const THEIA_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(7);

async fn bounded_shutdown<F: std::future::Future<Output = ()>>(step: F, name: &'static str) {
    if tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, step)
        .await
        .is_err()
    {
        tracing::warn!(subsystem = name, "shutdown timed out");
    }
}

async fn stop_theia_for_exit(app: &tauri::AppHandle) -> Result<(), String> {
    app.state::<theia::TheiaServer>().begin_shutdown();
    let handle = app.clone();
    let stop = tauri::async_runtime::spawn_blocking(move || {
        handle.state::<theia::TheiaServer>().stop();
    });
    #[cfg(windows)]
    match tokio::time::timeout(THEIA_SHUTDOWN_TIMEOUT, stop).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("IDE 关闭任务失败：{error}")),
        Err(_) => Err("IDE 进程清理超时，应用已保留，请稍候重试".into()),
    }
    #[cfg(not(windows))]
    stop.await
        .map_err(|error| format!("IDE 关闭任务失败：{error}"))
}

async fn required_shutdown<F: std::future::Future<Output = Result<(), String>>>(
    step: F,
    name: &'static str,
) -> Result<(), String> {
    tokio::time::timeout(SHUTDOWN_STEP_TIMEOUT, step)
        .await
        .map_err(|_| format!("{name} 本地进程清理超时，应用已保留，请稍候重试"))?
        .map_err(|error| format!("{name} 本地进程清理失败，应用已保留：{error}"))
}

async fn shutdown_before_exit(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    tracing::info!("application shutdown started");
    // Start the runtime's session flush immediately. It has its own bounded
    // bound; a second two-second timeout would cancel it before persistence.
    let (theia, automation, _, runtime) = tokio::join!(
        stop_theia_for_exit(app),
        required_shutdown(automation::shutdown_all(), "Browser/Computer Use"),
        bounded_shutdown(weixin::shutdown(app), "weixin"),
        commands::stop_agent_runtime(&state),
    );
    let errors = [theia.err(), automation.err(), runtime.err()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if errors.is_empty() {
        tracing::info!("application shutdown completed");
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

fn try_begin_exit(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn renderer_save_confirmation_required(
    has_window: bool,
    listener_ready: bool,
    has_recording: bool,
) -> bool {
    has_window && (listener_ready || has_recording)
}

async fn prepare_recording_for_exit(app: &tauri::AppHandle) -> Result<(), String> {
    if webview_recovery::unavailable(app) {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        let (sender, answer) = tokio::sync::oneshot::channel();
        app.dialog().message("界面尚未恢复，无法确认界面内的未保存修改。已写入本机的会话和录音会保留；可以取消退出并重试恢复，或保存现有录音后退出。")
            .title("退出 EchoAgent")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("保存现有录音并退出".into(), "取消退出".into()))
            .show(move |accepted| { let _ = sender.send(accepted); });
        if !answer.await.unwrap_or(false) {
            return Err("已取消退出，可从系统托盘重试恢复界面".into());
        }
        meeting_capture::finish_before_exit().await?;
        return meeting_minutes::recover_renderer_recordings().map(|_| ());
    }
    let listener_ready = EXIT_LISTENER_READY.load(Ordering::Acquire);
    // Before the renderer has ever installed its recording guard, a failed or
    // still-loading startup with no recording must remain able to quit.
    let has_recording = if listener_ready {
        false
    } else {
        meeting_minutes::meeting_list()?
            .iter()
            .any(|record| matches!(record.status.as_str(), "recording" | "paused"))
    };
    if renderer_save_confirmation_required(
        app.get_webview_window("main").is_some(),
        listener_ready,
        has_recording,
    ) {
        let request_id = uuid::Uuid::now_v7().to_string();
        let notice = ExitPreparationNotice {
            request_id: request_id.clone(),
            recording_saved: false,
        };
        let (sender, response) = tokio::sync::oneshot::channel();
        {
            let mut pending = EXIT_PREPARATION.lock().map_err(|_| "退出准备状态不可用")?;
            if pending.is_some() {
                return Err("正在准备退出，请稍候".into());
            }
            *pending = Some((request_id.clone(), sender));
        }
        *ACTIVE_EXIT_REQUEST
            .lock()
            .map_err(|_| "退出准备状态不可用")? = Some(notice.clone());
        let result = if let Err(error) = app.emit("desktop://prepare-exit", &notice) {
            Err(format!("无法通知录音保存：{error}"))
        } else {
            match tokio::time::timeout(Duration::from_secs(30), response).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("录音保存确认已中断，请重试退出".into()),
                Err(_) => Err(
                    "退出准备确认超时，应用已保留。请完成草稿或录音的保存确认后重试退出。".into(),
                ),
            }
        };
        {
            let mut pending = EXIT_PREPARATION.lock().map_err(|_| "退出准备状态不可用")?;
            if pending.as_ref().is_some_and(|(id, _)| id == &request_id) {
                pending.take();
            }
        }
        result?;
        if let Some(notice) = ACTIVE_EXIT_REQUEST
            .lock()
            .map_err(|_| "退出准备状态不可用")?
            .as_mut()
        {
            if notice.request_id == request_id {
                notice.recording_saved = true;
            }
        }
    }
    meeting_capture::finish_before_exit().await
}

fn resolve_exit_preparation(
    pending: &mut Option<(String, tokio::sync::oneshot::Sender<ExitPreparationResult>)>,
    request_id: &str,
    error: Option<String>,
) -> bool {
    if !pending.as_ref().is_some_and(|(id, _)| id == request_id) {
        return false;
    }
    let Some((_, sender)) = pending.take() else {
        return false;
    };
    sender.send(error.map_or(Ok(()), Err)).is_ok()
}

#[tauri::command]
fn desktop_exit_prepared(
    window: tauri::WebviewWindow,
    request_id: String,
    error: Option<String>,
) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("只能由主窗口确认录音保存".into());
    }
    if request_id.len() > 128 || error.as_ref().is_some_and(|error| error.len() > 4096) {
        return Err("退出准备响应无效或过长".into());
    }
    let mut pending = EXIT_PREPARATION.lock().map_err(|_| "退出准备状态不可用")?;
    Ok(resolve_exit_preparation(&mut pending, &request_id, error))
}

#[tauri::command]
fn desktop_exit_listener_ready(
    window: tauri::WebviewWindow,
) -> Result<Option<ExitPreparationNotice>, String> {
    if window.label() != "main" {
        return Err("只能由主窗口注册录音保存监听".into());
    }
    EXIT_LISTENER_READY.store(true, Ordering::Release);
    ACTIVE_EXIT_REQUEST
        .lock()
        .map_err(|_| "退出准备状态不可用".to_string())
        .map(|notice| notice.clone())
}

pub(crate) fn abort_prepared_exit(app: &tauri::AppHandle, error: String, runtime_stopped: bool) {
    host_watchdog::cancel_exit();
    let request_id = ACTIVE_EXIT_REQUEST
        .lock()
        .ok()
        .and_then(|mut active| active.take())
        .map(|notice| notice.request_id);
    EXIT_READY.store(false, Ordering::Release);
    if runtime_stopped {
        app.state::<theia::TheiaServer>().cancel_shutdown();
        let _ = app.emit("desktop://runtime-restart-required", ());
    }
    EXIT_IN_PROGRESS.store(false, Ordering::Release);
    show_main_window(app);
    let message = format!("未退出，应用已保留：{error}");
    tracing::error!(%message, "exit preparation failed");
    let _ = app.emit(
        "desktop://exit-failed",
        serde_json::json!({ "requestId": request_id, "message": message }),
    );
}

/// Read native registries and live actors rather than the user-visible menu.
/// Archived/hidden sessions still execute, and a recovering WebView can miss
/// activity events entirely. Bound the read so a failed Runtime stays quittable.
async fn application_exit_activity(
    app: &tauri::AppHandle,
) -> Result<desktop_tray_model::ExitActivity, String> {
    let state = app.state::<AppState>();
    let tx = state.tx.lock().map_err(|_| "Runtime 状态不可读")?.clone();
    let permissions = app.state::<Permissions>();
    let questions = app.state::<Questions>();
    let plans = app.state::<PlanApprovals>();
    let trusts = app.state::<FolderTrusts>();
    let runtime = async {
        let Some(tx) = tx else {
            return Ok::<_, String>(desktop_tray_model::ExitActivity::default());
        };
        let snapshot: desktop_tray_model::RuntimeExitSnapshot = ext::call_ext(
            &tx,
            "echo.agent/sessions/list",
            ext::raw_params(&serde_json::json!({})),
        )
        .await
        .map_err(|error| format!("读取 Runtime 活动失败：{error}"))?;
        if !state.is_runtime_current(&tx) {
            return Err("Runtime 已切换，活动状态需要重新确认".into());
        }
        Ok(snapshot.activity())
    };
    let groups = async {
        tauri::async_runtime::spawn_blocking(groups::group_list)
            .await
            .map_err(|error| format!("读取专家团活动失败：{error}"))?
    };
    let (runtime, permissions, questions, plans, trusts, approvals, groups) = tokio::join!(
        runtime,
        permissions.list(None),
        questions.list(None),
        plans.list(None),
        trusts.list(None),
        automation::automation_pending_approvals(None),
        groups,
    );
    let mut activity = runtime?;
    // A dispatching group may not have created its Runtime session yet.
    activity.running |= groups?
        .iter()
        .any(|group| group.works.iter().any(|work| work.active()));
    activity.waiting |= !permissions.is_empty()
        || !questions.is_empty()
        || !plans.is_empty()
        || !trusts.is_empty()
        // An uninitialized optional browser/computer service has no approvals.
        || approvals.is_ok_and(|approvals| !approvals.is_empty());
    Ok(activity)
}

pub(crate) async fn prepare_application_exit(app: &tauri::AppHandle) -> Result<(), String> {
    if !try_begin_exit(&EXIT_IN_PROGRESS) {
        return Err("应用正在退出、恢复或更新，请稍候".into());
    }
    let activity = tokio::time::timeout(Duration::from_secs(3), application_exit_activity(app))
        .await
        .unwrap_or_else(|_| Err("后台任务状态读取超时".into()));
    if !desktop_tray::confirm_active_exit(app, activity).await {
        EXIT_IN_PROGRESS.store(false, Ordering::Release);
        return Err("已取消退出，任务继续运行".into());
    }
    host_watchdog::prepare_exit();
    webview_recovery::pause_for_exit(app);
    if let Err(error) = prepare_recording_for_exit(app).await {
        abort_prepared_exit(app, error.clone(), false);
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn shutdown_application_for_exit(app: &tauri::AppHandle) -> Result<(), String> {
    shutdown_before_exit(app).await
}

pub(crate) fn complete_prepared_exit(app: tauri::AppHandle, restart: bool) {
    host_watchdog::finish_exit();
    EXIT_READY.store(true, Ordering::Release);
    if restart {
        app.restart();
    } else {
        app.exit(0);
    }
}

fn request_graceful_exit(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        if prepare_application_exit(&app).await.is_err() {
            return;
        }
        if let Err(error) = shutdown_before_exit(&app).await {
            abort_prepared_exit(&app, error, true);
            return;
        }
        complete_prepared_exit(app, false);
    });
}

/// AppKit's predefined Quit invokes `terminate:` directly. Tao then delivers
/// LoopDestroyed, without a preventable ExitRequested event. Route the menu
/// and Cmd+Q through the same save/child-process barrier as tray/window exit.
#[cfg(target_os = "macos")]
fn application_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app)?;
    let items = menu.items()?;
    let application = items
        .first()
        .and_then(MenuItemKind::as_submenu)
        .ok_or_else(|| std::io::Error::other("macOS application menu is missing"))?;
    let application_items = application.items()?;
    let quit_index = application_items
        .len()
        .checked_sub(1)
        .filter(|index| matches!(application_items[*index], MenuItemKind::Predefined(_)))
        .ok_or_else(|| std::io::Error::other("macOS default Quit menu is missing"))?;
    application.remove_at(quit_index)?;
    application.append(&MenuItem::with_id(
        app,
        "echoagent.app.quit",
        "退出 EchoAgent",
        true,
        Some("CmdOrCtrl+Q"),
    )?)?;
    Ok(menu)
}

async fn restart_prepared_application(app: tauri::AppHandle) -> Result<(), String> {
    if let Err(error) = process_supervisor::permit_application_relaunch() {
        let message = format!("无法重启应用：{error}");
        abort_prepared_exit(&app, message.clone(), false);
        return Err(message);
    }
    if let Err(error) = shutdown_before_exit(&app).await {
        abort_prepared_exit(&app, error.clone(), true);
        return Err(error);
    }
    tauri::async_runtime::spawn(async move {
        complete_prepared_exit(app, true);
    });
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const DESKTOP_MIN_WIDTH: f64 = 1024.0;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const DESKTOP_MIN_HEIGHT: f64 = 680.0;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;

    if app
        .try_state::<webview_recovery::RendererRecovery>()
        .is_some_and(|state| webview_recovery::desktop_recovery_status(state).phase_is_failed())
        || app.get_webview_window("main").is_none()
    {
        webview_recovery::retry(app);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        host_watchdog::visibility(true);
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[derive(Debug, PartialEq)]
struct AutostartLaunchSpec {
    name: String,
    args: Vec<String>,
}

fn profile_hash(home: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(home.as_os_str().as_encoded_bytes())[..8])
}

fn autostart_launch_spec(
    package_name: &str,
    custom_home: Option<&std::path::Path>,
) -> Result<AutostartLaunchSpec, String> {
    let mut spec = AutostartLaunchSpec {
        name: package_name.into(),
        args: vec!["--background".into()],
    };
    if let Some(home) = custom_home {
        if !home.is_absolute() {
            return Err("登录启动数据目录必须是绝对路径".into());
        }
        let home_arg = home.to_str().ok_or("数据目录无法转换为系统登录启动参数")?;
        spec.name = format!("{package_name}-profile-{}", profile_hash(home));
        spec.args.extend(["--data-home".into(), home_arg.into()]);
    }
    Ok(spec)
}

fn custom_profile_identifier(base: &str, home: &std::path::Path, validation: bool) -> String {
    format!(
        "{base}.{}-{}",
        if validation { "review" } else { "profile" },
        profile_hash(home)
    )
}

fn validate_validation_profile_launch(validation: bool, redirected: bool) -> Result<(), String> {
    if validation && !redirected {
        Err("验收模式必须指定独立的数据目录；不能使用默认数据目录或指向它的符号链接。请通过 --data-home <临时绝对路径> 重新启动。".into())
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn validate_custom_profile_macos_version(
    custom_profile: bool,
    reported_version: Option<&str>,
) -> Result<(), String> {
    if !custom_profile {
        return Ok(());
    }
    let unknown = "无法识别 macOS 版本，已阻止自定义数据目录启动以保护现有资料；请确认系统版本，或使用默认数据目录。";
    let version = reported_version.ok_or(unknown)?.trim();
    if version.is_empty() || version.len() > 64 {
        return Err(unknown.into());
    }
    let mut components = version.split('.');
    let major = components
        .next()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(unknown)?;
    if components.any(|value| value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(unknown.into());
    }
    if major < 14 {
        return Err(format!(
            "自定义数据目录的独立 WebView 存储需要 macOS 14 或更新版本，当前为 {version}；已阻止启动以保护现有资料。请升级系统，或使用默认数据目录。"
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_custom_profile_macos_support() -> Result<(), String> {
    if paths::redirected_data_home().is_none() {
        return Ok(());
    }
    let version = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    validate_custom_profile_macos_version(true, version.as_deref())
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct DesktopWindowState {
    width: f64,
    height: f64,
    maximized: bool,
    #[serde(default)]
    position: Option<(i32, i32)>,
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn desktop_window_state_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    paths::desktop_data_dir(app)
        .ok()
        .map(|dir| dir.join("window-state.json"))
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn save_desktop_window_state(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let Ok(maximized) = window.is_maximized() else {
        return;
    };
    let Ok(scale_factor) = window.scale_factor() else {
        return;
    };
    let Ok(size) = window.inner_size() else {
        return;
    };
    let logical = size.to_logical::<f64>(scale_factor);
    if logical.width < DESKTOP_MIN_WIDTH || logical.height < DESKTOP_MIN_HEIGHT {
        return;
    }
    let Some(path) = desktop_window_state_path(app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let state = DesktopWindowState {
        width: logical.width,
        height: logical.height,
        maximized,
        position: window.outer_position().ok().map(|p| (p.x, p.y)),
    };
    if let Ok(payload) = serde_json::to_vec(&state) {
        let _ = crate::paths::write_private_file(&path, &payload);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn restore_desktop_window_state(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    use tauri::{LogicalSize, Size};

    let Some(path) = desktop_window_state_path(app) else {
        return;
    };
    let Ok(payload) = crate::shell_fs::read_regular_file_bounded(&path, 16 * 1024) else {
        return;
    };
    let Ok(state) = serde_json::from_slice::<DesktopWindowState>(&payload) else {
        return;
    };

    let mut max_width = 4096.0;
    let mut max_height = 2160.0;
    if let Ok(Some(monitor)) = window.current_monitor() {
        if let Ok(scale_factor) = window.scale_factor() {
            let screen = monitor.size().to_logical::<f64>(scale_factor);
            max_width = (screen.width * 0.92).max(DESKTOP_MIN_WIDTH);
            max_height = (screen.height * 0.92).max(DESKTOP_MIN_HEIGHT);
        }
    }

    let width = state.width.clamp(DESKTOP_MIN_WIDTH, max_width);
    let height = state.height.clamp(DESKTOP_MIN_HEIGHT, max_height);
    let _ = window.set_size(Size::Logical(LogicalSize::new(width, height)));
    let _ = window.center();
    if let Some((x, y)) = state.position {
        let visible = window
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .any(|monitor| {
                let origin = monitor.position();
                let size = monitor.size();
                i64::from(x) + 64 >= i64::from(origin.x)
                    && i64::from(y) + 64 >= i64::from(origin.y)
                    && i64::from(x) < i64::from(origin.x) + i64::from(size.width) - 64
                    && i64::from(y) < i64::from(origin.y) + i64::from(size.height) - 64
            });
        if visible {
            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
    if state.maximized {
        let _ = window.maximize();
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn bind_main_window_lifecycle(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    use tauri::{DragDropEvent, Manager, WindowEvent};
    use tauri_plugin_notification::NotificationExt;
    restore_desktop_window_state(app, window);
    let app_handle = app.clone();
    let close_window = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
            let access = app_handle.state::<shell_fs::FilesystemAccess>();
            // Mirror the stats command's hard batch cap so one oversized
            // native drop cannot consume the process-wide exact-file grant
            // budget for paths the Composer will never inspect.
            for path in paths.iter().take(shell_fs::MAX_PICKED_FILES) {
                if !attachment_blob::is_supported_attachment_path(path) {
                    continue;
                }
                if let Err(error) = access.authorize_file(path) {
                    tracing::warn!(path = %path.display(), %error, "failed to authorize dropped file");
                }
            }
        }
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            save_desktop_window_state(&app_handle, &close_window);
        }
        WindowEvent::CloseRequested { api, .. } => {
            save_desktop_window_state(&app_handle, &close_window);
            api.prevent_close();
            let close_to_tray = desktop_preferences::load(&app_handle)
                .map(|preferences| preferences.close_to_tray)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to read desktop preferences; keeping app alive");
                    true
                });
            if close_to_tray {
                match close_window.hide() {
                    Ok(()) => {
                        host_watchdog::visibility(false);
                        if desktop_preferences::take_background_notice(&app_handle)
                            .unwrap_or_else(|error| {
                                tracing::warn!(%error, "failed to persist background notice state");
                                false
                            })
                        {
                            if let Err(error) = app_handle
                                .notification()
                                .builder()
                                .title("EchoAgent 已在后台运行")
                                .body("从系统托盘选择“打开 EchoAgent”可恢复窗口；选择“退出 EchoAgent”可完全退出。")
                                .show()
                            {
                                tracing::warn!(%error, "failed to show background residency notification");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to hide main window");
                        show_main_window(&app_handle);
                    }
                }
            } else {
                request_graceful_exit(app_handle.clone());
            }
        }
        _ => {}
    });
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn setup_desktop_lifecycle(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::Manager;
    #[cfg(target_os = "macos")]
    use tauri_plugin_autostart::MacosLauncher;
    let custom_home = paths::redirected_data_home();
    let launch = autostart_launch_spec(&app.package_info().name, custom_home.as_deref())?;
    let autostart = tauri_plugin_autostart::Builder::new()
        .app_name(launch.name)
        .args(launch.args);
    #[cfg(target_os = "macos")]
    let autostart = autostart.macos_launcher(MacosLauncher::LaunchAgent);
    app.handle().plugin(autostart.build())?;
    automations::sync_autostart_state(app.handle());

    desktop_tray::setup(app.handle())?;

    if let Some(window) = app.get_webview_window("main") {
        bind_main_window_lifecycle(app.handle(), &window);
        webview_recovery::install(app.handle(), &window).map_err(std::io::Error::other)?;
        if std::env::args_os().any(|arg| arg == "--background") {
            let _ = window.hide();
        }
    }
    webview_recovery::start_monitor(app.handle().clone());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if host_watchdog::run_helper_if_requested() {
        return;
    }
    host_watchdog::consume_restore_marker();
    if let Err(error) = paths::apply_startup_data_home() {
        eprintln!("EchoAgent 启动参数无效：{error}");
        std::process::exit(2);
    }
    let validation = std::env::var("ECHO_DESKTOP_REVIEW").as_deref() == Ok("1")
        || std::env::var("ECHO_DESKTOP_VALIDATION").as_deref() == Ok("1");
    if let Err(error) =
        validate_validation_profile_launch(validation, paths::redirected_data_home().is_some())
    {
        eprintln!("EchoAgent 无法启动：{error}");
        std::process::exit(2);
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = ensure_custom_profile_macos_support() {
        eprintln!("EchoAgent 无法启动：{error}");
        std::process::exit(2);
    }
    // Keep the non-blocking file writer alive through the whole Tauri event
    // loop. Release builds on Windows have no console, so this is the primary
    // post-mortem record for startup, model reload and session admission.
    let _logging_guard = logging::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = env!("ECHOAGENT_BUILD_COMMIT"),
        build_time = env!("ECHOAGENT_BUILD_TIME"),
        log_dir = %logging::log_dir().display(),
        "EchoAgent process started"
    );

    if let Err(error) = paths::initialize_runtime_home() {
        tracing::error!(%error, "failed to initialize EchoAgent runtime home");
    }
    process_supervisor::install_host_job();
    echo_agent_tools::util::process_safety::install(process_identity::snapshot);
    let mut context = tauri::generate_context!();
    let mut profile_windows = Vec::new();
    if let Some(home) = paths::redirected_data_home() {
        // Initialization created this root. Resolve aliases and relative env
        // paths once so OS login and manual launches share the same profile.
        let home = home.canonicalize().unwrap_or(home);
        std::env::set_var("ECHO_AGENT_HOME", &home);
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
        let mut store_id = [0_u8; 16];
        store_id.copy_from_slice(&digest[..16]);
        // Keep WebView drafts, queues and other localStorage away from the
        // ordinary profile. macOS 14+ uses a dedicated WKWebsiteDataStore.
        for window in &mut context.config_mut().app.windows {
            window.data_directory = Some(home.join("webview"));
            window.data_store_identifier = Some(store_id);
            if window.create {
                profile_windows.push((window.clone(), home.join("webview"), store_id));
                // This Tauri release's WindowConfig conversion does not copy
                // the data-directory/store fields. Build redirected windows
                // explicitly so these options reach the actual WebView.
                window.create = false;
            }
        }
        // Default, custom and explicit acceptance profiles cannot focus each
        // other's resident process; restarting the same profile still reuses it.
        let identifier = custom_profile_identifier(&context.config().identifier, &home, validation);
        context.config_mut().identifier = identifier;
    }
    let builder = tauri::Builder::default();
    // Tauri requires single-instance to be the first registered plugin. A
    // second launch focuses the resident window (which may be tray-hidden)
    // instead of creating another WebView against the same profile.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if !args.iter().any(|arg| arg == "--background") {
            show_main_window(app);
        }
    }));

    #[cfg(target_os = "macos")]
    let builder = builder
        .on_web_content_process_terminate(|webview| {
            if webview.label() == "main" {
                let app = webview.app_handle();
                let epoch =
                    webview_recovery::desktop_recovery_status(app.state()).window_generation();
                webview_recovery::request(app.clone(), epoch, webview_recovery::Failure::Renderer);
            }
        })
        .menu(application_menu)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "echoagent.app.quit" {
                request_graceful_exit(app.clone());
            }
        });

    let app = builder
        .setup(move |app| {
            if let Err(error) = host_watchdog::start() {
                tracing::warn!(%error, "host crash recovery could not start; renderer recovery remains available");
            }
            // Match the ordinary auto-created window order, including focus
            // requests received while a large pending backup is being restored.
            for (config, directory, store_id) in profile_windows {
                tauri::WebviewWindowBuilder::from_config(&*app, &config)?
                    .data_directory(directory)
                    .data_store_identifier(store_id)
                    .build()?;
            }
            backup::apply_pending(app.handle()).map_err(std::io::Error::other)?;
            if let Err(error) = meeting_minutes::recover_host_recordings() {
                tracing::error!(%error, "startup could not finalize interrupted recordings");
            }
            // 离线租约过期时在 Agent Runtime 启动前移除受管 Skill 注入，
            // 避免已撤权的长期离线客户继续加载企业能力。
            org::enforce_skill_lease();

            // Team MCP server（127.0.0.1 streamable-http）：同步 bind 后台 accept。
            // 必须在任何 new_session 之前 —— 端口即刻写入 BOUND_PORT 供传参。
            if let Err(error) = team_mcp::reconcile_orphaned_teams() {
                tracing::warn!(%error, "session team repair failed; retry on next startup");
            }
            team_mcp::serve();
            team_mcp::set_app_handle(app.handle().clone());
            if let Err(error)=groups::initialize(app.handle().clone()) {tracing::error!(%error,"group service initialization failed; existing data retained");}
            if let Err(error) = org_mcp::clear_persisted_registration() {
                tracing::warn!(%error, "failed to remove legacy organization MCP registration");
            }

            // Register the bounded, application-owned paste store so all
            // supported historical attachments remain previewable after a
            // process restart without exhausting exact-file grants.
            // Done here (not in `FilesystemAccess::new`) because the allow-list
            // is registered before setup runs and needs `app_data_dir` from the
            // Tauri handle.
            if let Ok(clipboard_dir) = paths::desktop_data_dir(app.handle()) {
                let access = app.state::<shell_fs::FilesystemAccess>();
                access.register_attachment_store(&clipboard_dir.join("clipboard-images"));
            }
            // The authenticated knowledge bridge is reachable on loopback for
            // personal local knowledge. Organization tools are added only after
            // a verified login and shared-scope bootstrap.
            if let Err(error) = automation::serve(app.handle().clone()) {
                // Automation is an optional capability. Keep the desktop usable
                // and expose the degraded state through automation_capabilities
                // instead of aborting the native launch callback.
                tracing::error!(%error, "automation MCP server failed to start");
            }
            if let Err(error) = office_mcp::serve(app.handle().clone()) {
                tracing::error!(%error, "office MCP server failed to start");
            }
            if let Err(error) = live_info_mcp::serve() {
                tracing::error!(%error, "live information MCP server failed to start");
            }
            org_mcp::serve(app.handle().clone());
            org::start_background_sync(app.handle().clone());
            personal_knowledge::start_background_index(app.handle().clone());
            weixin::init(app.handle());
            #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
            setup_desktop_lifecycle(app)?;
            Ok(())
        })
        .on_page_load(|webview, payload| {
            if let Err(error) = native_exit_validation::on_page_load(webview, payload) {
                tracing::error!(%error, "isolated native exit review could not start");
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(webview_recovery::RendererRecovery::default())
        .manage(AppState::default())
        .manage(desktop_tray::DesktopTray::default())
        .manage(backup::BackupState::default())
        .manage(Permissions::new())
        .manage(Questions::new())
        .manage(PlanApprovals::new())
        .manage(FolderTrusts::new())
        .manage(shell_fs::FilesystemAccess::new())
        .manage(coding_workspace::CodingProcesses::new())
        .manage(theia::TheiaServer::default())
        .manage(coding::watcher::WatcherRegistry::default())
        .manage(org::shared_state())
        .manage(weixin::WeixinState::default())
        .invoke_handler(tauri::generate_handler![
            desktop_tray::desktop_tray_sync,
            desktop_tray::desktop_tray_pending_action,
            desktop_tray::desktop_tray_ack_action,
            weixin::weixin_status,
            weixin::weixin_qr_start,
            weixin::weixin_qr_poll,
            weixin::weixin_set_workspaces,
            weixin::weixin_revoke_session,
            weixin::weixin_handoff,
            weixin::weixin_disconnect,
            webview_recovery::desktop_recovery_status,
            webview_recovery::desktop_renderer_heartbeat,
            desktop_validation::desktop_validation_ready,
            desktop_validation::desktop_review_complete,
            desktop_validation::desktop_review_context,
            native_browser_validation::desktop_review_browser_manifest,
            native_core_validation::desktop_review_core_manifest,
            native_core_validation::desktop_review_model_connection,
            native_exit_validation::native_exit_review_context,
            native_ide_validation::desktop_review_ide_manifest,
            desktop_exit_prepared,
            desktop_exit_listener_ready,
            desktop_preferences::desktop_notification_preview_save,
            notifications::notification_take_pending_opens,
            notifications::notification_set_visible_session,
            backup::backup_export,
            backup::backup_inspect,
            backup::backup_restore,
            backup::backup_restored_ui,
            backup::backup_acknowledge_ui,
            backup::backup_last_error,
            // session lifecycle
            commands::agent_init,
            commands::agent_auth_status,
            commands::agent_new_session,
            commands::agent_prepare_task_workspace,
            commands::agent_session_workspace_status,
            commands::agent_load_session,
            commands::agent_list_sessions,
            commands::agent_list_all_sessions,
            commands::agent_set_model,
            commands::agent_list_workspaces,
            commands::agent_set_knowledge_sources,
            commands::agent_send,
            commands::agent_cancel,
            commands::agent_shutdown,
            commands::agent_resolve_permission,
            commands::agent_resolve_question,
            commands::agent_resolve_plan_approval,
            commands::agent_list_pending_interactions,
            commands::agent_rename_session,
            commands::agent_delete_session,
            commands::agent_set_session_pinned,
            commands::agent_set_session_archived,
            commands::agent_set_session_status,
            commands::agent_set_session_expert,
            commands::agent_clear_session_expert,
            // signed desktop application updates (private organization CA)
            app_updater::app_update_check,
            app_updater::app_update_install,
            // desktop window lifecycle preferences
            desktop_preferences::desktop_preferences_get,
            desktop_preferences::desktop_preferences_save,
            // context usage pill (echo.agent/session/info + echo.agent/session/usage)
            commands::agent_session_info,
            commands::agent_session_usage,
            // BYOK providers (~/.echo-agent/config.toml [model.*])
            providers::providers_list,
            providers::providers_save_provider,
            providers::providers_save_connection,
            providers::providers_save_model,
            providers::providers_delete_provider,
            providers::providers_delete_model,
            providers::providers_fetch_models,
            providers::providers_fetch_models_for_provider,
            providers::providers_test_connection,
            providers::providers_test_model_connection,
            // Deprecated shims kept registered for back-compat.
            providers::providers_save,
            providers::providers_delete,
            // default permission rules (~/.echo-agent/config.toml [permission])
            permission_config::permission_list,
            permission_config::permission_save,
            // task-scoped permission mode (sidecar metadata + targeted live notify)
            permission_config::permission_mode_get,
            permission_config::permission_mode_set,
            // agent/assistant defaults (~/.echo-agent/config.toml [models].default + [ui].default_selected_permission)
            permission_config::agents_defaults_get,
            permission_config::agents_defaults_save,
            // authoritative local policy store
            policy::policy_get,
            policy::policy_save,
            // subagents config (~/.echo-agent/config.toml [subagents].max_depth)
            agent_config::subagents_config_get,
            agent_config::subagents_config_save,
            // web search config (~/.echo-agent/config.toml [models].web_search)
            agent_config::web_search_config_get,
            agent_config::web_search_config_save,
            agent_config::memory_config_get,
            agent_config::memory_config_save,
            // skills (echo.agent/skills/*)
            skills::skills_list,
            skills::skills_add,
            skills::skills_remove,
            skills::skills_toggle,
            skill_installer::skills_inspect_package,
            skill_installer::skills_inspect_packages,
            skill_installer::skills_install_package,
            skill_installer::skills_uninstall_package,
            // organization server connection, RAG, documents, and managed Skills
            org::org_login,
            org::org_logout,
            org::org_session,
            org::org_sync_model_config,
            org::org_bootstrap,
            org::org_list_scopes,
            org::org_submit_document,
            org::org_document_submissions_mine,
            org::org_document_submissions_mine_page,
            org::org_remove_own_submission,
            org::org_preview_document,
            org::org_download_document,
            org::org_scan_document_folder,
            org::org_list_documents,
            org::org_list_memories,
            org::org_memory_promotions_mine,
            org::org_submit_memory_candidate,
            org::org_document_status,
            org::org_fetch_document,
            org::org_archive_document,
            org::org_new_document_version,
            org::org_publish_document,
            org::org_list_skills,
            org::org_skill_detail,
            org::org_set_skill_preference,
            org::org_publish_skill,
            org::org_submit_skill,
            org::org_skill_submissions_mine,
            org::org_sync_skills,
            org::org_qa_feedback,
            org::org_local_kb_sources_get,
            org::org_local_kb_sources_set,
            org::personal_knowledge_allowed,
            personal_knowledge::personal_knowledge_search,
            personal_knowledge::personal_knowledge_cancel_search,
            personal_knowledge::personal_knowledge_rebuild,
            personal_knowledge::personal_knowledge_index_status,
            // connectors / MCP (echo.agent/mcp/*)
            mcp::mcp_list,
            mcp::mcp_upsert,
            mcp::mcp_delete,
            mcp::mcp_toggle,
            mcp::mcp_setup,
            mcp::mcp_toggle_tool,
            mcp::mcp_config_path,
            mcp::mcp_config_read,
            mcp::mcp_config_save,
            mcp::mcp_auth_trigger,
            mcp::mcp_auth_status,
            // experts / assistants (~/.echo-agent/agents/*.md)
            agents_store::agents_list,
            agents_store::agents_get,
            agents_store::agents_save,
            agents_store::agents_update,
            agents_store::agents_merge_definition,
            agents_store::agents_validate_references,
            agents_store::agents_delete,
            agents_store::agents_template,
            knowledge_traces::knowledge_traces_load,
            knowledge_traces::knowledge_trace_save,
            session_artifacts::session_exports_list,
            session_artifacts::session_exports_catalog,
            session_artifacts::session_export_authorize,
            expert_avatar::expert_avatar,
            // Safe, bounded previews for local chat image attachments.
            attachment_preview::attachment_thumbnail,
            // Persist clipboard file blobs under app_data_dir and
            // hand back a path so the existing attachments pipeline (multimodal
            // send, thumbnails, ACP metadata) keeps working.
            attachment_blob::save_attachment_blob,
            attachment_blob::discard_attachment_blob,
            // Durable long-form recording, MiniMax ASR, minutes and exports.
            meeting_minutes::meeting_create,
            meeting_capture::meeting_capture_support,
            meeting_capture::meeting_capture_start,
            meeting_capture::meeting_capture_status,
            meeting_capture::meeting_capture_pause,
            meeting_capture::meeting_capture_stop,
            meeting_minutes::meeting_check_connection,
            meeting_minutes::meeting_append_pcm,
            meeting_minutes::meeting_set_paused,
            meeting_minutes::meeting_finish_recording,
            meeting_minutes::meeting_import_audio,
            meeting_minutes::meeting_process,
            meeting_minutes::meeting_regenerate_minutes,
            meeting_minutes::meeting_update,
            meeting_minutes::meeting_list,
            meeting_minutes::meeting_get,
            meeting_minutes::meeting_job_active,
            meeting_minutes::meeting_export,
            document_export::document_export,
            meeting_minutes::meeting_delete,
            meeting_minutes::meeting_open_audio,
            // EchoAgent admin: memory / search / rewind / commands / plan / tasks / reload
            agent_admin::memory_list,
            agent_admin::memory_records_get,
            agent_admin::memory_record_save,
            agent_admin::memory_record_retire,
            agent_admin::memory_get,
            agent_admin::memory_save,
            agent_admin::memory_append,
            agent_admin::memory_delete,
            agent_admin::memory_clear_session_summaries,
            agent_admin::memory_rewrite,
            agent_admin::memory_flush,
            agent_admin::memory_dream,
            agent_admin::session_search,
            agent_admin::rewind_points,
            agent_admin::rewind_execute,
            agent_admin::session_fork,
            agent_admin::commands_list,
            agent_admin::prompt_history,
            agent_admin::tasks_list,
            agent_admin::task_kill,
            agent_admin::folder_trust_respond,
            agent_admin::set_coding_mode,
            agent_admin::set_plan_mode,
            agent_admin::toggle_plan_mode,
            agent_admin::internal_reload,
            agent_admin::plugins_list,
            agent_admin::plugins_action,
            // first-party Browser Use / Computer Use runtime and safety UI
            automation::automation_capabilities,
            automation::automation_status,
            automation::automation_pause,
            automation::automation_resume,
            automation::automation_stop,
            automation::automation_clear_browser_data,
            automation::automation_set_private_network,
            automation::automation_request_computer_permissions,
            automation::automation_pending_approvals,
            automation::automation_resolve_approval,
            // authoritative team runtime registry (shared with the MCP tools)
            team_mcp::team_snapshot,
            team_mcp::team_create,
            team_mcp::team_disband,
            groups::group_create,
            groups::group_list,
            groups::group_snapshot,
            groups::group_owner,
            groups::group_send,
            groups::group_control,
            groups::group_settings,
            groups::group_metadata,
            groups::group_delete,
            groups::group_cleanup_pending,
            groups::group_cleanup_ack,
            groups::management::group_member_change,
            groups::management::group_team_templates,
            groups::management::group_team_save,
            groups::management::group_team_delete,
            groups::management::group_decision,
            groups::management::group_decision_change,
            // notification center
            notifications::notification_append,
            notifications::notification_list,
            notifications::notification_mark_read,
            notifications::notification_mark_all_read,
            notifications::notification_clear,
            notifications::notify_channels_list,
            notifications::notify_desktop_status,
            notifications::notify_desktop_set_enabled,
            notifications::notify_desktop_test,
            notifications::notify_channel_upsert,
            notifications::notify_channel_remove,
            notifications::notify_channel_set_enabled,
            notifications::notify_channel_test,
            // automations (local scheduler)
            automations::automations_snapshot,
            automations::automations_save,
            automations::automations_delete,
            automations::automations_set_status,
            automations::automations_run,
            automations::automation_records_archive,
            automations::automation_records_delete,
            // shell / filesystem (markdown links, path click, apply write)
            resource_identity::filesystem_resource_identity,
            shell_fs::open_url,
            shell_fs::filesystem_pick_directory,
            shell_fs::filesystem_pick_files,
            shell_fs::filesystem_attachment_stats,
            shell_fs::open_path,
            shell_fs::reveal_in_folder,
            shell_fs::path_stat,
            shell_fs::read_text_file,
            shell_fs::read_file_base64,
            shell_fs::write_text_file,
            shell_fs::export_text_file,
            shell_fs::list_dir,
            shell_fs::browse_directory,
            shell_fs::echo_agent_data_dir,
            shell_fs::open_echo_agent_data_dir,
            // dedicated Coding Workspace: bounded repository analysis and
            // user-initiated validation commands inside an authorized root
            coding::task::coding_task_list,
            coding::task::coding_task_create,
            coding::task::coding_task_get,
            coding::task::coding_task_delete,
            coding::task::coding_task_rename,
            coding::task::coding_task_bind_runtime,
            coding::task::coding_task_set_context,
            coding::task::coding_task_confirm_acceptance,
            coding::changeset::coding_changeset_get,
            coding::changeset::coding_changeset_capture_baseline,
            coding::changeset::coding_changeset_diff,
            coding::changeset::coding_changeset_mark_reviewed,
            coding::changeset::coding_changeset_record_change,
            coding::changeset::coding_changeset_discard_file,
            coding::changeset::coding_changeset_sync,
            coding::changeset::coding_task_rollback,
            coding::verification::coding_verification_detect,
            coding::verification::coding_verification_list,
            coding::verification::coding_verification_approve_plan_command,
            coding::verification::coding_verification_run,
            coding::verification::coding_verification_cancel,
            coding::diagnostics::coding_diagnostics_list,
            // phase 2 cross-file symbol index (Task 17)
            coding::symbols::coding_index_status,
            coding::symbols::coding_index_rebuild,
            coding::symbols::coding_symbol_query,
            coding::symbols::coding_symbol_at,
            coding::watcher::coding_index_bootstrap,
            coding::watcher::coding_index_release,
            // phase 2 reference search (Task 19)
            coding::refs::coding_refs_find,
            coding::refs::coding_refs_definition,
            // phase 2 impact analysis (Task 20)
            coding::impact::coding_impact_analyze,
            coding::orchestrator::coding_task_submit_requirement,
            coding::orchestrator::coding_task_sync_plan,
            coding::orchestrator::coding_task_execution_ledger,
            coding::orchestrator::coding_task_report_start_failed,
            coding::orchestrator::coding_task_begin_followup,
            coding::orchestrator::coding_task_report_interrupted,
            coding::orchestrator::coding_task_resume,
            coding::orchestrator::coding_task_begin_verification,
            coding::orchestrator::coding_orchestrator_report_implementation,
            coding::orchestrator::coding_orchestrator_report_verification,
            coding::orchestrator::coding_orchestrator_state,
            coding::delivery::coding_delivery_report,
            coding::delivery::coding_delivery_commit_input,
            coding::delivery::coding_delivery_pr_input,
            coding::delivery::coding_git_commit,
            coding::delivery::coding_delivery_commit_hunks,
            coding::isolation::coding_isolation_create,
            coding::isolation::coding_isolation_info,
            coding::isolation::coding_isolation_integrate,
            coding::review::coding_review_confirm,
            coding::tdd::coding_tdd_status,
            coding::tdd::coding_tdd_record_red,
            coding::tdd::coding_tdd_waive,
            coding::verification::coding_verification_config_get,
            coding::verification::coding_verification_config_save,
            coding::changeset::coding_changeset_revert_hunk_preview,
            coding::changeset::coding_checkpoint_start,
            coding::changeset::coding_checkpoint_preserve_buffer,
            coding::task::coding_task_for_session,
            coding::changeset::coding_checkpoint_finish,
            coding::changeset::coding_checkpoint_list,
            coding::changeset::coding_checkpoint_restore,
            coding::delivery::coding_git_commit_history,
            coding::isolation::coding_isolation_list,
            coding::isolation::coding_isolation_cleanup,
            coding_closed_documents::coding_closed_document_snapshot,
            coding_closed_documents::coding_closed_document_apply,
            coding_document_bridge::coding_document_tool_respond,
            coding_document_bridge::coding_document_tool_is_active,
            coding_document_bridge::coding_document_cancel_ack,
            coding_document_bridge::coding_document_cancellation_state,
            coding_external_tools::coding_external_tools_pending,
            coding_external_tools::coding_external_tool_recover,
            coding_document_bridge::coding_document_editor_attach,
            coding_document_bridge::coding_document_editor_present,
            coding_document_bridge::coding_document_editor_detach,
            coding_completion::coding_inline_observe,
            coding::verification::coding_verification_freshness,
            coding::review::coding_review_list,
            coding_plan::coding_plan_save,
            coding_patch::coding_document_patch_preview,
            coding_completion::coding_inline_complete,
            coding_completion::coding_inline_cancel,
            coding_workspace::coding_analyze_workspace,
            coding_workspace::coding_git_snapshot,
            coding_workspace::coding_git_diff,
            coding_workspace::coding_git_set_staged,
            coding_workspace::coding_read_document,
            coding_workspace::coding_write_document,
            coding_workspace::coding_create_entry,
            coding_workspace::coding_delete_entries,
            coding_workspace::coding_rename_entry,
            coding_workspace::coding_copy_entries,
            coding_workspace::coding_move_entries,
            coding_workspace::coding_restore_from_trash,
            coding_workspace::coding_search_workspace,
            coding_workspace::coding_terminal_create,
            coding_workspace::coding_terminal_write,
            coding_workspace::coding_terminal_resize,
            coding_workspace::coding_terminal_close,
            theia::coding_theia_start,
            // durable local project metadata (renderer localStorage is only a cache)
            projects::projects_load,
            projects::projects_save,
            projects::project_assets_import,
            projects::project_assets_use,
            projects::project_asset_make_dir,
            projects::project_asset_remove,
            projects::project_assets_remove_all,
            // persisted WebDAV cloud storage
            storage::storage_providers_list,
            storage::storage_provider_upsert,
            storage::storage_provider_remove,
            storage::storage_provider_test,
            storage::storage_list,
            storage::storage_read_text,
            storage::storage_write_text,
            storage::storage_upload_file,
            storage::storage_download_file,
            storage::storage_delete,
            storage::storage_make_dir,
        ])
        .build(context)
        .expect("error while building EchoAgent");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = &event {
            if !EXIT_READY.load(Ordering::Acquire) {
                api.prevent_exit();
                if !webview_recovery::is_recovering(app_handle) {
                    request_graceful_exit(app_handle.clone());
                }
            }
        }

        // Closing the last window does not terminate the process on macOS.
        // Clicking the Dock icon must therefore restore the resident window.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } = event
        {
            show_main_window(app_handle);
        }
    });
}

#[cfg(test)]
mod exit_tests {
    use super::{renderer_save_confirmation_required, resolve_exit_preparation, try_begin_exit};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn validation_profiles_reject_default_home_before_any_startup_write() {
        assert!(super::validate_validation_profile_launch(true, false).is_err());
        assert!(super::validate_validation_profile_launch(true, true).is_ok());
        assert!(super::validate_validation_profile_launch(false, false).is_ok());
        assert!(super::validate_validation_profile_launch(false, true).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn validation_profiles_reject_a_symlink_to_the_default_home() {
        let directory = tempfile::tempdir().unwrap();
        let default = directory.path().join("默认 数据");
        std::fs::create_dir(&default).unwrap();
        let alias = directory.path().join("验收别名");
        std::os::unix::fs::symlink(&default, &alias).unwrap();
        let redirected = crate::paths::is_redirected_data_home(&alias, Some(&default));
        assert!(!redirected);
        assert!(super::validate_validation_profile_launch(true, redirected).is_err());
        let custom = directory.path().join("独立目录");
        assert!(super::validate_validation_profile_launch(
            true,
            crate::paths::is_redirected_data_home(&custom, Some(&default))
        )
        .is_ok());
    }

    #[test]
    fn macos_profile_guard_allows_supported_versions_and_leaves_default_unchanged() {
        for version in ["14", "14.0", "15.8.1\n", "26.0"] {
            assert!(super::validate_custom_profile_macos_version(true, Some(version)).is_ok());
        }
        assert!(super::validate_custom_profile_macos_version(false, None).is_ok());
        assert!(super::validate_custom_profile_macos_version(false, Some("13.6.9")).is_ok());
    }

    #[test]
    fn macos_profile_guard_never_silently_uses_the_default_store() {
        let unsupported =
            super::validate_custom_profile_macos_version(true, Some("13.6.9")).unwrap_err();
        assert!(unsupported.contains("需要 macOS 14"));
        for version in [
            None,
            Some(""),
            Some("Darwin 23"),
            Some("14..0"),
            Some("14.0\n13.0"),
        ] {
            let error = super::validate_custom_profile_macos_version(true, version).unwrap_err();
            assert!(error.contains("无法识别 macOS 版本"));
        }
    }

    #[test]
    fn default_autostart_arguments_and_name_remain_compatible() {
        let spec = super::autostart_launch_spec("echoagent", None).unwrap();
        assert_eq!(spec.name, "echoagent");
        assert_eq!(spec.args, ["--background"]);
    }

    #[test]
    fn custom_autostart_reopens_the_exact_home_without_touching_default_item() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap().join("中文 空格");
        let spec = super::autostart_launch_spec("echoagent", Some(&home)).unwrap();
        assert!(spec.name.starts_with("echoagent-profile-"));
        assert_ne!(spec.name, "echoagent");
        assert_eq!(
            crate::paths::parse_data_home_args(spec.args.iter().map(std::ffi::OsString::from))
                .unwrap(),
            Some(home.clone())
        );
        let other = super::autostart_launch_spec("echoagent", Some(&home.join("other"))).unwrap();
        assert_ne!(spec.name, other.name);
    }

    #[test]
    fn custom_and_review_instances_have_stable_separate_identifiers() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap();
        let ordinary = super::custom_profile_identifier("com.echoagent.desktop", &home, false);
        let review = super::custom_profile_identifier("com.echoagent.desktop", &home, true);
        assert_ne!(ordinary, "com.echoagent.desktop");
        assert_ne!(ordinary, review);
        assert_eq!(
            ordinary,
            super::custom_profile_identifier("com.echoagent.desktop", &home, false)
        );
        assert_ne!(
            ordinary,
            super::custom_profile_identifier("com.echoagent.desktop", &home.join("other"), false)
        );
    }

    #[test]
    fn graceful_exit_can_only_start_once() {
        let flag = AtomicBool::new(false);
        assert!(try_begin_exit(&flag));
        assert!(!try_begin_exit(&flag));
    }

    #[tokio::test]
    async fn stale_recording_reply_does_not_consume_the_retry_request() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut pending = Some(("retry-request".to_string(), sender));
        assert!(!resolve_exit_preparation(
            &mut pending,
            "timed-out-request",
            None
        ));
        assert!(pending.is_some());
        assert!(resolve_exit_preparation(
            &mut pending,
            "retry-request",
            None
        ));
        assert!(pending.is_none());
        assert_eq!(receiver.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn recording_save_failure_is_returned_and_can_be_retried() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut pending = Some(("first".to_string(), sender));
        assert!(resolve_exit_preparation(
            &mut pending,
            "first",
            Some("磁盘已满".into())
        ));
        assert_eq!(receiver.await.unwrap(), Err("磁盘已满".into()));
        assert!(pending.is_none());
    }

    #[test]
    fn uninitialized_empty_renderer_does_not_prevent_exit() {
        assert!(!renderer_save_confirmation_required(true, false, false));
        assert!(renderer_save_confirmation_required(true, false, true));
        assert!(renderer_save_confirmation_required(true, true, false));
        assert!(!renderer_save_confirmation_required(false, true, true));
    }

    #[tokio::test]
    async fn local_process_shutdown_timeout_is_a_retryable_failure() {
        assert!(super::required_shutdown(std::future::pending(), "fixture")
            .await
            .is_err());
        assert!(super::required_shutdown(async { Ok(()) }, "fixture")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn local_process_shutdown_error_cannot_become_a_successful_exit() {
        let result =
            super::required_shutdown(async { Err("拒绝停止浏览器".into()) }, "fixture").await;
        assert!(result.unwrap_err().contains("拒绝停止浏览器"));
    }
}
