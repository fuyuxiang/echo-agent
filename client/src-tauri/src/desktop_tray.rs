//! Native menu rendering and acknowledged navigation shared by macOS and Windows.
use crate::desktop_tray_model::{
    self as model, ExitActivity, PendingAction, TaskKind, TrayAction, TraySnapshot, TrayTask,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

const TRAY_ID: &str = "main-tray";
const NEW: &str = "echo.tray.new";
const SHOW: &str = "echo.tray.show";
const ALL: &str = "echo.tray.all";
const QUIT: &str = "echo.tray.quit";

#[derive(Default)]
struct MenuState {
    snapshot: TraySnapshot,
    rendered: Option<TraySnapshot>,
    generation: u64,
    // Keep previous bindings until queued OS selection events have arrived.
    // A menu item id is never reassigned to a different task.
    bindings: HashMap<String, TrayTask>,
    generations: VecDeque<Vec<String>>,
    pending: PendingAction,
}

#[derive(Default)]
pub struct DesktopTray {
    inner: Mutex<MenuState>,
    queued: AtomicBool,
}

fn main_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("只能由主窗口更新托盘任务".into())
    }
}

#[tauri::command]
pub fn desktop_tray_sync(
    window: tauri::WebviewWindow,
    snapshot: TraySnapshot,
    app: tauri::AppHandle,
) -> Result<(), String> {
    main_window(&window)?;
    snapshot.validate()?;
    let state = app.state::<DesktopTray>();
    state.inner.lock().map_err(|_| "托盘状态不可用")?.snapshot = snapshot;
    request_refresh(&app);
    Ok(())
}

#[tauri::command]
pub fn desktop_tray_pending_action(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, DesktopTray>,
) -> Result<Option<TrayAction>, String> {
    main_window(&window)?;
    Ok(state
        .inner
        .lock()
        .map_err(|_| "托盘状态不可用")?
        .pending
        .peek())
}

#[tauri::command]
pub fn desktop_tray_ack_action(
    window: tauri::WebviewWindow,
    id: String,
    state: tauri::State<'_, DesktopTray>,
) -> Result<bool, String> {
    main_window(&window)?;
    Ok(state
        .inner
        .lock()
        .map_err(|_| "托盘状态不可用")?
        .pending
        .acknowledge(&id))
}

fn navigate(app: &tauri::AppHandle, kind: &str, target: Option<String>) {
    if let Ok(mut state) = app.state::<DesktopTray>().inner.lock() {
        state.pending.replace(kind, target);
    }
    crate::show_main_window(app);
    // The pending action remains available if React is still starting or its
    // WebView is being recovered. The event is only a wake-up, never the data store.
    if let Err(error) = app.emit_to("main", "desktop://tray-action", ()) {
        tracing::warn!(%error, "tray navigation retained for renderer recovery");
    }
}

fn on_menu_event(app: &tauri::AppHandle, id: &str) {
    match id {
        SHOW => crate::show_main_window(app),
        NEW => navigate(app, "new", None),
        ALL => navigate(app, "all", None),
        QUIT => crate::request_graceful_exit(app.clone()),
        _ => {
            let task = app
                .state::<DesktopTray>()
                .inner
                .lock()
                .ok()
                .and_then(|state| state.bindings.get(id).cloned());
            if let Some(task) = task {
                navigate(
                    app,
                    match task.kind {
                        TaskKind::Session => "session",
                        TaskKind::Group => "group",
                    },
                    Some(task.id),
                );
            }
        }
    }
}

fn build_menu(app: &tauri::AppHandle, state: &mut MenuState) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    let mut bindings = Vec::new();
    let generation = state.generation + 1;
    if !state.snapshot.tasks.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "echo.tray.heading",
            state.snapshot.heading(),
            false,
            None::<&str>,
        )?)?;
        for (index, task) in state.snapshot.tasks.iter().enumerate() {
            let id = format!("echo.tray.task.{generation}.{index}");
            menu.append(&MenuItem::with_id(
                app,
                id.clone(),
                model::task_label(task, cfg!(windows)),
                true,
                None::<&str>,
            )?)?;
            bindings.push((id, task.clone()));
        }
        menu.append(&MenuItem::with_id(
            app,
            ALL,
            "查看全部任务…",
            true,
            None::<&str>,
        )?)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&MenuItem::with_id(app, NEW, "新对话", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(
        app,
        SHOW,
        "打开 EchoAgent",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        QUIT,
        "退出 EchoAgent",
        true,
        None::<&str>,
    )?)?;
    state.generation = generation;
    state
        .generations
        .push_back(bindings.iter().map(|(id, _)| id.clone()).collect());
    state.bindings.extend(bindings);
    while state.generations.len() > 16 {
        if let Some(old) = state.generations.pop_front() {
            for id in old {
                state.bindings.remove(&id);
            }
        }
    }
    Ok(menu)
}

/// Called on the UI thread. Defer replacement throughout native menu tracking,
/// including keyboard navigation; pointer-enter/leave alone cannot detect it.
fn menu_tracking() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetGUIThreadInfo, GUITHREADINFO, GUI_INMENUMODE, GUI_POPUPMENUMODE,
        };
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(0, &mut info) != 0
            && info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE) != 0
    }
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::{class, msg_send, runtime::AnyObject};
        let run_loop: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
        let mode: *mut objc2_foundation::NSString = msg_send![run_loop, currentMode];
        !mode.is_null() && (&*mode).to_string() == "NSEventTrackingRunLoopMode"
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    false
}

fn request_refresh(app: &tauri::AppHandle) {
    if app
        .state::<DesktopTray>()
        .queued
        .swap(true, Ordering::AcqRel)
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let handle = app.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            let result = app.run_on_main_thread(move || {
                if menu_tracking() {
                    let _ = send.send(false);
                    return;
                }
                let tray_state = handle.state::<DesktopTray>();
                // Hold the snapshot lock until queued is cleared. A newer sync
                // then schedules another render instead of being lost mid-render.
                if let Ok(mut state) = tray_state.inner.lock() {
                    if state.rendered.as_ref() != Some(&state.snapshot) {
                        let result = (|| -> tauri::Result<()> {
                            if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                                let menu = build_menu(&handle, &mut state)?;
                                tray.set_menu(Some(menu))?;
                                tray.set_tooltip(Some(state.snapshot.tooltip()))?;
                                state.rendered = Some(state.snapshot.clone());
                            }
                            Ok(())
                        })();
                        if let Err(error) = result {
                            tracing::error!(%error, "native tray refresh failed");
                        }
                    }
                    tray_state.queued.store(false, Ordering::Release);
                } else {
                    tray_state.queued.store(false, Ordering::Release);
                }
                let _ = send.send(true);
            });
            if result.is_err() {
                app.state::<DesktopTray>()
                    .queued
                    .store(false, Ordering::Release);
                break;
            }
            if receive.await.unwrap_or(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
}

fn icon() -> Result<tauri::image::Image<'static>, image::ImageError> {
    #[cfg(target_os = "macos")]
    let bytes = include_bytes!("../icons/tray/macos@2x.png").as_slice();
    #[cfg(not(target_os = "macos"))]
    let bytes = include_bytes!("../icons/tray/windows-64.png").as_slice();
    let image = image::load_from_memory(bytes)?.to_rgba8();
    #[cfg(windows)]
    let image = {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        image::imageops::resize(&image, size, size, image::imageops::FilterType::Lanczos3)
    };
    let (width, height) = image.dimensions();
    Ok(tauri::image::Image::new_owned(
        image.into_raw(),
        width,
        height,
    ))
}

pub fn setup(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let menu = {
        let state = app.state::<DesktopTray>();
        let mut state = state.inner.lock().map_err(|_| "托盘状态不可用")?;
        build_menu(app, &mut state)?
    };
    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .icon(icon()?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("EchoAgent")
        .show_menu_on_left_click(!cfg!(windows))
        .on_menu_event(|app, event| on_menu_event(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            if cfg!(windows)
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                )
            {
                crate::show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub async fn confirm_active_exit(
    app: &tauri::AppHandle,
    activity: Result<ExitActivity, String>,
) -> bool {
    let description = match activity {
        Ok(activity) if !activity.has_tasks() => return true,
        Ok(activity) => format!(
            "当前{}。退出将中断这些任务，并停止本机定时任务调度；已保存的会话会保留。",
            activity.description()
        ),
        Err(error) => {
            tracing::warn!(%error, "could not inspect native activity before exit");
            "暂时无法确认后台任务状态。退出可能中断未完成的任务，并停止本机定时任务调度；已保存的会话会保留。".into()
        }
    };
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let (send, receive) = tokio::sync::oneshot::channel();
    crate::show_main_window(app);
    app.dialog()
        .message(description)
        .title("退出 EchoAgent？")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "退出 EchoAgent".into(),
            "继续运行".into(),
        ))
        .show(move |accepted| {
            let _ = send.send(accepted);
        });
    receive.await.unwrap_or(false)
}
