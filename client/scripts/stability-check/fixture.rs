use crate::{
    host_watchdog, process_identity, process_safety, process_supervisor, webview_recovery,
    EXIT_IN_PROGRESS,
};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{atomic::Ordering, Mutex};
use tauri::Manager;

struct Fixture {
    profile: PathBuf,
    nonce: String,
    writer: Mutex<()>,
}
fn steps() -> &'static [&'static str] {
    if cfg!(windows) {
        &["rebuild", "renderer", "browser", "host", "restart"]
    } else {
        &["rebuild", "reload", "host", "restart"]
    }
}
fn read_step(profile: &std::path::Path) -> Result<usize, String> {
    match std::fs::read_to_string(profile.join("step")) {
        Ok(value) => value
            .parse()
            .map_err(|e| format!("invalid fixture step: {e}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.to_string()),
    }
}
fn record(app: &tauri::AppHandle, value: Value) -> Result<(), String> {
    let state = app.state::<Fixture>();
    let _writer = state.writer.lock().map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.profile.join("evidence.jsonl"))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(
        &mut file,
        &json!({ "pid": std::process::id(), "value": value }),
    )
    .map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
#[tauri::command]
fn fixture_config(state: tauri::State<'_, Fixture>) -> Result<Value, String> {
    Ok(json!({"nonce":state.nonce, "steps":steps(), "step":read_step(&state.profile)?}))
}
#[tauri::command]
fn fixture_report(app: tauri::AppHandle, value: Value) -> Result<(), String> {
    let failed = value.get("error").is_some();
    record(&app, value)?;
    if failed {
        host_watchdog::finish_exit();
        EXIT_IN_PROGRESS.store(true, Ordering::Release);
        app.exit(1);
    }
    Ok(())
}
#[tauri::command]
fn fixture_finish(app: tauri::AppHandle) -> Result<(), String> {
    record(&app, json!({"complete":true}))?;
    host_watchdog::finish_exit();
    EXIT_IN_PROGRESS.store(true, Ordering::Release);
    app.exit(0);
    Ok(())
}
#[tauri::command]
async fn fixture_fail(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    nonce: String,
    kind: String,
) -> Result<(), String> {
    crate::native_review_scope::require_main_internal(&window)?;
    if nonce != app.state::<Fixture>().nonce {
        return Err("wrong private fixture identity".into());
    }
    let status = webview_recovery::desktop_recovery_status(app.state());
    // Test orchestration must survive an immediate host kill independently of
    // the browser's asynchronous localStorage disk flush. The draft remains
    // exclusively in localStorage so its recovery is still tested end to end.
    let state = app.state::<Fixture>();
    let step = read_step(&state.profile)?;
    if steps().get(step).copied() != Some(kind.as_str()) {
        return Err("unexpected fixture failure step".into());
    }
    let mut marker =
        std::fs::File::create(state.profile.join("step")).map_err(|e| e.to_string())?;
    write!(marker, "{}", step + 1).map_err(|e| e.to_string())?;
    marker.sync_all().map_err(|e| e.to_string())?;
    drop(state);
    record(&app, json!({"fault":kind}))?;
    match kind.as_str() {
        "host" => std::process::exit(73),
        "restart" => {
            // Match production restart: the new host must survive closure of
            // the current host's kill-on-close Windows Job.
            process_supervisor::permit_application_relaunch().map_err(|e| e.to_string())?;
            host_watchdog::finish_exit();
            EXIT_IN_PROGRESS.store(true, Ordering::Release);
            app.restart();
        }
        "rebuild" => webview_recovery::request(
            app,
            status.window_generation(),
            webview_recovery::Failure::Browser,
        ),
        "reload" => webview_recovery::request(
            app,
            status.window_generation(),
            webview_recovery::Failure::Renderer,
        ),
        #[cfg(windows)]
        "browser" | "renderer" => {
            window
                .with_webview(move |view| {
                    let result = (|| -> Result<(), String> {
                        let core = unsafe { view.controller().CoreWebView2() }
                            .map_err(|e| e.to_string())?;
                        crate::windows_injection::terminate_owned_webview(&core, kind == "renderer")
                    })();
                    if let Err(error) = result {
                        let _ = record(&app, json!({"error":error}));
                        host_watchdog::finish_exit();
                        EXIT_IN_PROGRESS.store(true, Ordering::Release);
                        app.exit(1);
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        _ => return Err("unknown fixed failure injection".into()),
    }
    Ok(())
}

pub fn run() {
    if host_watchdog::run_helper_if_requested() {
        return;
    }
    host_watchdog::consume_restore_marker();
    let profile = std::env::var_os("ECHO_STABILITY_PROFILE")
        .map(PathBuf::from)
        .expect("private fixture profile required");
    let profile = profile.canonicalize().unwrap();
    if profile.join("stop").exists() {
        return;
    }
    assert!(profile.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    let nonce = profile.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(nonce.starts_with("echo-stability-") && nonce.len() > 18);
    process_safety::install(process_identity::snapshot);
    process_supervisor::install_host_job();
    let mut context = tauri::generate_context!();
    let mut store = [0u8; 16];
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    profile.hash(&mut hash);
    store[..8].copy_from_slice(&hash.finish().to_le_bytes());
    store[8..].copy_from_slice(&hash.finish().to_be_bytes());
    context.config_mut().app.windows[0].data_directory = Some(profile.join("webview"));
    context.config_mut().app.windows[0].data_store_identifier = Some(store);
    let builder = tauri::Builder::default()
        .manage(Fixture {
            profile,
            nonce,
            writer: Mutex::new(()),
        })
        .manage(webview_recovery::RendererRecovery::default())
        .plugin(tauri_plugin_dialog::init());
    #[cfg(target_os = "macos")]
    let builder = builder.on_web_content_process_terminate(|webview| {
        let app = webview.app_handle();
        let epoch = webview_recovery::desktop_recovery_status(app.state()).window_generation();
        webview_recovery::request(app.clone(), epoch, webview_recovery::Failure::Renderer);
    });
    let app = builder
        .setup(|app| {
            record(app.handle(), json!({"nativeSetup":true})).map_err(std::io::Error::other)?;
            let handle = app.handle().clone();
            let profile = app.state::<Fixture>().profile.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    if profile.join("stop").exists() {
                        host_watchdog::finish_exit();
                        EXIT_IN_PROGRESS.store(true, Ordering::Release);
                        handle.exit(0);
                        break;
                    }
                }
            });
            host_watchdog::start().map_err(std::io::Error::other)?;
            let config = &app.config().app.windows[0];
            let window = tauri::WebviewWindowBuilder::from_config(app, config)?
                .data_directory(config.data_directory.clone().unwrap())
                .data_store_identifier(config.data_store_identifier.unwrap())
                .build()?;
            webview_recovery::install(app.handle(), &window).map_err(std::io::Error::other)?;
            record(app.handle(), json!({"windowInstalled":true})).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            fixture_config,
            fixture_report,
            fixture_finish,
            fixture_fail,
            webview_recovery::desktop_recovery_status,
            webview_recovery::desktop_renderer_heartbeat
        ])
        .build(context)
        .expect("isolated recovery fixture failed to start");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            let _ = record(app, json!({"normalExitObserved":true}));
        }
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !EXIT_IN_PROGRESS.load(Ordering::Acquire) && webview_recovery::is_recovering(app) {
                api.prevent_exit();
            }
        }
    });
}
