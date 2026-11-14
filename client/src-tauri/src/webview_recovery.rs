//! Native renderer recovery: remains available when JavaScript has disappeared.
use crate::recovery_policy::{Decision, RecoveryPolicy, MAX_ATTEMPTS};
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryStatus {
    phase: &'static str,
    attempt: usize,
    max_attempts: usize,
    message: String,
    window_generation: u64,
}
impl RecoveryStatus {
    pub fn phase_is_failed(&self) -> bool {
        self.phase == "failed"
    }
    pub fn window_generation(&self) -> u64 {
        self.window_generation
    }
}
struct Recovery {
    policy: RecoveryPolicy,
    status: RecoveryStatus,
    document: Option<String>,
    failed_document: Option<String>,
    awaiting_ready: bool,
    heartbeat: Instant,
    monitor_tick: Instant,
    recording_interrupted: bool,
}
pub struct RendererRecovery(Mutex<Recovery>);
impl Default for RendererRecovery {
    fn default() -> Self {
        let restored = crate::host_watchdog::restored();
        Self(Mutex::new(Recovery {
            policy: RecoveryPolicy::default(),
            status: RecoveryStatus {
                phase: if restored { "restored" } else { "ready" },
                attempt: 0,
                max_attempts: MAX_ATTEMPTS,
                message: if restored {
                    "应用已从异常退出中恢复，已保存的任务和草稿仍保留。请核对中断前的操作结果，系统不会自动重放。".into()
                } else {
                    String::new()
                },
                window_generation: 0,
            },
            document: None,
            failed_document: None,
            awaiting_ready: false,
            heartbeat: Instant::now(),
            monitor_tick: Instant::now(),
            recording_interrupted: false,
        }))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Failure {
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    Renderer,
    Browser,
    Unresponsive,
}

pub fn is_recovering(app: &tauri::AppHandle) -> bool {
    app.try_state::<RendererRecovery>().is_some_and(|state| {
        state
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .policy
            .current()
            .is_some()
    })
}

pub fn unavailable(app: &tauri::AppHandle) -> bool {
    app.try_state::<RendererRecovery>().is_some_and(|state| {
        let state = state.0.lock().unwrap_or_else(|e| e.into_inner());
        state.status.phase == "failed" || state.policy.current().is_some()
    })
}

pub fn pause_for_exit(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<RendererRecovery>() {
        let mut state = state.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(generation) = state.policy.current() {
            state.policy.finish(generation);
            state.awaiting_ready = false;
            state.status.phase = "failed";
            state.status.message = "界面恢复已因退出暂停，可取消退出后重试恢复。".into();
        }
    }
}

fn publish(app: &tauri::AppHandle, status: &RecoveryStatus) {
    let _ = app.emit("desktop://renderer-recovery", status);
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(if status.phase == "recovering" {
            "EchoAgent - 正在恢复界面"
        } else if status.phase == "failed" {
            "EchoAgent - 界面恢复失败，点击重试"
        } else {
            "EchoAgent - 后台运行中"
        }));
    }
}

#[tauri::command]
pub fn desktop_recovery_status(state: tauri::State<'_, RendererRecovery>) -> RecoveryStatus {
    state
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .status
        .clone()
}

#[tauri::command]
pub fn desktop_renderer_heartbeat(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, RendererRecovery>,
    document_id: String,
    window_generation: u64,
) -> Result<RecoveryStatus, String> {
    if crate::native_review_scope::require_main_internal(&window).is_err() {
        #[cfg(not(debug_assertions))]
        let dev = false;
        #[cfg(debug_assertions)]
        let dev =
            if let (Ok(url), Some(config)) = (window.url(), app.config().build.dev_url.as_ref()) {
                window.label() == "main"
                    && url.scheme() == config.scheme()
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                    && matches!(config.host_str(), Some("localhost" | "127.0.0.1"))
                    && url.port_or_known_default() == config.port_or_known_default()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == config.path()
            } else {
                false
            };
        if !dev {
            return Err("仅主应用页面可确认恢复状态".into());
        }
    }
    if window.label() != "main" || document_id.is_empty() || document_id.len() > 80 {
        return Err("无效的界面恢复身份".into());
    }
    let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
    if window_generation != recovery.status.window_generation {
        return Err("旧窗口的恢复回执已忽略".into());
    }
    recovery.heartbeat = Instant::now();
    let mut changed = false;
    if recovery.awaiting_ready && recovery.failed_document.as_deref() != Some(document_id.as_str())
    {
        if let Some(generation) = recovery.policy.current() {
            recovery.policy.finish(generation);
            recovery.status.phase = "restored";
            recovery.status.message =
                "界面已恢复，已保存的任务和草稿仍保留。已执行的操作不会自动重放。".into();
            if recovery.recording_interrupted {
                recovery
                    .status
                    .message
                    .push_str(" 麦克风录音已中断，请在会议中核对已保存内容。");
            }
            recovery.awaiting_ready = false;
            changed = true;
            tracing::info!(generation, "desktop renderer recovered");
        }
    }
    recovery.document = Some(document_id);
    let status = recovery.status.clone();
    drop(recovery);
    if changed {
        publish(&app, &status);
    }
    Ok(status)
}

pub fn retry(app: &tauri::AppHandle) {
    if crate::EXIT_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let epoch = {
        let state = app.state::<RendererRecovery>();
        let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
        if recovery.policy.current().is_some() {
            return;
        }
        recovery.policy.manual_retry();
        recovery.status.phase = "ready";
        recovery.status.window_generation
    };
    request(app.clone(), epoch, Failure::Browser);
}

fn failed(app: &tauri::AppHandle, message: &str) {
    let state = app.state::<RendererRecovery>();
    let status = {
        let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
        recovery.status.phase = "failed";
        recovery.status.message = message.into();
        recovery.status.clone()
    };
    publish(app, &status);
    // A React toast cannot be rendered by a dead WebView. Use a native dialog,
    // and leave tray/menu recovery available if the user dismisses it.
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let handle = app.clone();
    app.dialog().message("界面自动恢复未完成，已有会话文件仍保留在本机。可重试恢复；正在执行的操作不会自动重放。若仍失败，请保留应用日志以便诊断。")
        .title("EchoAgent 界面恢复")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("重试恢复".into(), "暂不重试".into()))
        .show(move |accepted| { if accepted { retry(&handle); } });
}

pub fn request(app: tauri::AppHandle, epoch: u64, kind: Failure) {
    if crate::EXIT_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let decision = {
        let state = app.state::<RendererRecovery>();
        let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
        if epoch != recovery.status.window_generation {
            return;
        }
        if recovery.status.phase == "failed" {
            return;
        }
        let decision = recovery.policy.begin(Instant::now());
        if let Decision::Retry { attempt, .. } = decision {
            crate::EXIT_LISTENER_READY.store(false, std::sync::atomic::Ordering::Release);
            recovery.failed_document = recovery.document.clone();
            recovery.awaiting_ready = false;
            recovery.status.phase = "recovering";
            recovery.status.attempt = attempt;
            recovery.status.message =
                format!("正在恢复界面（{attempt}/{MAX_ATTEMPTS}），原会话会在重新连接后显示。");
        }
        decision
    };
    match decision {
        Decision::Busy => (),
        Decision::Exhausted => {
            failed(&app, "界面短时间内多次异常，自动恢复已暂停。请重试恢复。");
        }
        Decision::Retry {
            generation, delay, ..
        } => {
            tracing::warn!(?kind, generation, "desktop renderer recovery scheduled");
            let status = desktop_recovery_status(app.state());
            publish(&app, &status);
            // Do not create/destroy WebView2 from inside its COM event handler.
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(delay).await;
                if crate::EXIT_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
                    app.state::<RendererRecovery>()
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .policy
                        .finish(generation);
                    return;
                }
                {
                    let state = app.state::<RendererRecovery>();
                    let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
                    if recovery.policy.current() != Some(generation) {
                        return;
                    }
                    recovery.awaiting_ready = true;
                }
                let handle = app.clone();
                let rebuild =
                    tauri::async_runtime::spawn_blocking(move || recover_window(&handle, kind))
                        .await;
                let error = match rebuild {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    tracing::error!(%error, generation, "desktop renderer recovery attempt failed");
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                let retry_needed = {
                    let state = app.state::<RendererRecovery>();
                    let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
                    recovery.policy.finish(generation)
                };
                if retry_needed {
                    let epoch = desktop_recovery_status(app.state()).window_generation();
                    request(app, epoch, Failure::Browser);
                }
            });
        }
    }
}

fn recover_window(app: &tauri::AppHandle, kind: Failure) -> Result<(), String> {
    let window = app.get_webview_window("main");
    if matches!(kind, Failure::Renderer) {
        if let Some(window) = &window {
            recover_recordings(app);
            window.reload().map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    let (visible, focused) = window
        .as_ref()
        .map(|w| {
            (
                w.is_visible().unwrap_or(true),
                w.is_focused().unwrap_or(false),
            )
        })
        .unwrap_or((true, false));
    if let Some(window) = window {
        crate::save_desktop_window_state(app, &window);
        window.destroy().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.get_webview_window("main").is_some() {
            if Instant::now() >= deadline {
                return Err("旧界面未能完成关闭".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if crate::EXIT_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
        return Err("应用正在退出".into());
    }
    recover_recordings(app);
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .ok_or("找不到主窗口配置")?;
    let mut builder = tauri::WebviewWindowBuilder::from_config(app, config)
        .map_err(|e| e.to_string())?
        .visible(visible)
        .focused(focused);
    if let Some(directory) = &config.data_directory {
        builder = builder.data_directory(directory.clone());
    }
    if let Some(identifier) = config.data_store_identifier {
        builder = builder.data_store_identifier(identifier);
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    crate::bind_main_window_lifecycle(app, &window);
    install(app, &window)?;
    Ok(())
}

fn recover_recordings(app: &tauri::AppHandle) {
    match crate::meeting_minutes::recover_renderer_recordings() {
        Ok(0) => {}
        result => {
            if let Err(error) = result {
                tracing::error!(%error, "interrupted microphone recording requires manual recovery");
            }
            app.state::<RendererRecovery>()
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .recording_interrupted = true;
        }
    }
}

pub fn install(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    let epoch = {
        let state = app.state::<RendererRecovery>();
        let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
        recovery.status.window_generation += 1;
        recovery.heartbeat = Instant::now();
        recovery.status.window_generation
    };
    #[cfg(windows)]
    {
        use webview2_com::{
            BrowserProcessExitedEventHandler,
            Microsoft::Web::WebView2::Win32::{
                ICoreWebView2Environment5, ICoreWebView2_2,
                COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_FAILED,
                COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL,
                COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
                COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
            },
            ProcessFailedEventHandler,
        };
        use windows_core::Interface;
        let handle = app.clone();
        window.with_webview(move |webview| {
            let result = (|| -> windows_core::Result<()> {
                let core = unsafe { webview.controller().CoreWebView2()? };
                let mut pid = 0;
                unsafe { core.BrowserProcessId(&mut pid)? };
                let protection = echo_agent_tools::util::process_safety::protect_tree(pid).unwrap_or_else(|error| {
                    tracing::warn!(%error, pid, "WebView2 identity lease unavailable; process inventory protection retained");
                    None
                });
                let browser_handle = handle.clone();
                let mut unresponsive: Option<(Instant, Instant)> = None;
                let callback = ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let _keep_protected = &protection;
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED;
                    unsafe { args.ProcessFailedKind(&mut kind)? };
                    tracing::error!(kind = kind.0, window_generation = epoch, "WebView2 process failed");
                    let failure = if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED { Some(Failure::Browser) }
                    else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED { Some(Failure::Renderer) }
                    else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE {
                        let now = Instant::now();
                        let (since, last) = unresponsive.get_or_insert((now, now));
                        if now.duration_since(*last) > Duration::from_secs(30) { *since = now; }
                        *last = now;
                        (now.duration_since(*since) >= Duration::from_secs(45)).then_some(Failure::Unresponsive)
                    } else { None };
                    if let Some(failure) = failure { request(handle.clone(), epoch, failure); }
                    Ok(())
                }));
                let mut token = 0;
                unsafe { core.add_ProcessFailed(&callback, &mut token)? };
                let environment: ICoreWebView2Environment5 = unsafe { core.cast::<ICoreWebView2_2>()?.Environment()? }.cast()?;
                let callback = BrowserProcessExitedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut kind = COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL;
                        unsafe { args.BrowserProcessExitKind(&mut kind)? };
                        if kind == COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_FAILED {
                            request(browser_handle.clone(), epoch, Failure::Browser);
                        }
                    }
                    Ok(())
                }));
                unsafe { environment.add_BrowserProcessExited(&callback, &mut token)? };
                Ok(())
            })();
            if let Err(error) = result { tracing::error!(%error, "failed to install WebView2 process protection/recovery"); }
        }).map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    let _ = (window, epoch);
    Ok(())
}

pub fn start_monitor(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            if crate::EXIT_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
                continue;
            }
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            let focused =
                window.is_focused().unwrap_or(false) && window.is_visible().unwrap_or(false);
            crate::host_watchdog::visibility(window.is_visible().unwrap_or(false));
            let failure_epoch = {
                let state = app.state::<RendererRecovery>();
                let mut recovery = state.0.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                // Suspension/background timer throttling is not a renderer crash.
                if !focused || now.duration_since(recovery.monitor_tick) > Duration::from_secs(25) {
                    recovery.heartbeat = now;
                }
                recovery.monitor_tick = now;
                (recovery.status.phase != "failed"
                    && now.duration_since(recovery.heartbeat) > Duration::from_secs(120))
                .then_some(recovery.status.window_generation)
            };
            if let Some(epoch) = failure_epoch {
                request(app.clone(), epoch, Failure::Unresponsive);
            }
        }
    });
}
