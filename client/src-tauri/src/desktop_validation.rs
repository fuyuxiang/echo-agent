//! Release gate: a real WebView renders the app and completes a native IPC call.
//! Only active in a deliberately isolated validation launch.
use tauri::Manager;

#[cfg(windows)]
async fn wait_for_theia_frontend_ready(
    log_path: &std::path::Path,
    offset: u64,
) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom};

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        if let Ok(mut file) = std::fs::File::open(log_path) {
            if file.metadata().map_err(|error| error.to_string())?.len() >= offset {
                file.seek(SeekFrom::Start(offset))
                    .map_err(|error| error.to_string())?;
                let mut output = Vec::new();
                file.read_to_end(&mut output)
                    .map_err(|error| error.to_string())?;
                if String::from_utf8_lossy(&output)
                    .contains("Frontend application startup sequence completed")
                {
                    return Ok(());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("Packaged IDE frontend did not become ready within 45 seconds".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(windows)]
mod console_windows {
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread,
        time::Duration,
    };
    use windows_sys::{
        core::BOOL,
        Win32::{
            Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM},
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            UI::WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
            },
        },
    };

    fn app_process_tree() -> HashSet<u32> {
        let root = std::process::id();
        let mut owned = HashSet::from([root]);
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return owned;
        }
        let mut parents = HashMap::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while found {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            found = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        loop {
            let before = owned.len();
            for (&pid, &parent) in &parents {
                if owned.contains(&parent) {
                    owned.insert(pid);
                }
            }
            if owned.len() == before {
                return owned;
            }
        }
    }

    fn visible() -> HashMap<(isize, u32), String> {
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
            if unsafe { IsWindowVisible(hwnd) } == 0 {
                return 1;
            }
            let mut class = [0u16; 128];
            let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            if length <= 0 {
                return 1;
            }
            let class = String::from_utf16_lossy(&class[..length as usize]);
            if class != "ConsoleWindowClass" && class != "CASCADIA_HOSTING_WINDOW_CLASS" {
                return 1;
            }
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            let windows = unsafe { &mut *(data as *mut HashMap<(isize, u32), String>) };
            windows.insert((hwnd as isize, pid), class);
            1
        }

        let mut windows = HashMap::new();
        unsafe { EnumWindows(Some(visit), (&mut windows as *mut _) as LPARAM) };
        windows
    }

    pub(super) struct Watch {
        stop: Arc<AtomicBool>,
        observed: Arc<Mutex<HashSet<String>>>,
        thread: thread::JoinHandle<()>,
    }

    impl Watch {
        pub(super) fn start() -> Self {
            let baseline: HashSet<_> = visible().into_keys().collect();
            let stop = Arc::new(AtomicBool::new(false));
            let observed = Arc::new(Mutex::new(HashSet::new()));
            let stop_in_thread = stop.clone();
            let observed_in_thread = observed.clone();
            let thread = thread::spawn(move || {
                let mut inspected = baseline;
                while !stop_in_thread.load(Ordering::Relaxed) {
                    let new_windows: Vec<_> = visible()
                        .into_iter()
                        .filter(|((handle, pid), _)| inspected.insert((*handle, *pid)))
                        .collect();
                    if !new_windows.is_empty() {
                        let owned = app_process_tree();
                        for ((handle, pid), class) in new_windows {
                            if !owned.contains(&pid) {
                                continue;
                            }
                            if let Ok(mut seen) = observed_in_thread.lock() {
                                seen.insert(format!("{class} PID={pid} HWND={handle}"));
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
            Self {
                stop,
                observed,
                thread,
            }
        }

        pub(super) fn finish(self) -> Vec<String> {
            self.stop.store(true, Ordering::Relaxed);
            let _ = self.thread.join();
            let mut windows: Vec<_> = self
                .observed
                .lock()
                .map(|seen| seen.iter().cloned().collect())
                .unwrap_or_default();
            windows.sort();
            windows
        }
    }
}

#[tauri::command]
pub async fn desktop_validation_ready(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    if std::env::var("ECHO_DESKTOP_VALIDATION").as_deref() != Ok("1") {
        return Ok(());
    }
    if window.label() != "main" || std::env::var_os("ECHO_AGENT_HOME").is_none() {
        return Err("Validation requires an isolated runtime home and the main window".into());
    }
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
    for relative in [
        "theia/browser/lib/backend/main.js",
        if cfg!(windows) {
            "theia/node/node.exe"
        } else {
            "theia/node/bin/node"
        },
        "office/worker.mjs",
        "office/fonts/NotoSansCJKsc-Regular.otf",
    ] {
        if !resources.join(relative).is_file() {
            return Err(format!("Packaged resource missing: {relative}"));
        }
    }
    #[cfg(windows)]
    let console_watch = console_windows::Watch::start();
    #[cfg(windows)]
    let cli_error = async {
        let git = crate::process_supervisor::background_sync_command("git")
            .arg("--version")
            .output()
            .map_err(|error| format!("Packaged Git command failed to start: {error}"))?;
        if !git.status.success() {
            return Err("Packaged Git command failed".to_string());
        }
        let git = crate::process_supervisor::background_async_command("git")
            .arg("--version")
            .output()
            .await
            .map_err(|error| format!("Packaged async Git command failed to start: {error}"))?;
        if !git.status.success() {
            return Err("Packaged async Git command failed".to_string());
        }
        Ok::<(), String>(())
    }
    .await
    .err();
    #[cfg(not(windows))]
    let cli_error: Option<String> = None;
    // Start the IDE from the packaged resource path. Staged-resource tests
    // cannot catch packaging errors such as missing native modules or lost
    // executable permissions on macOS.
    #[cfg(any(windows, target_os = "macos"))]
    let ide_error = async {
        let workspace = crate::paths::echo_agent_home_dir().join("中文项目验证");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let access = app.state::<crate::shell_fs::FilesystemAccess>();
        let authorized = access.authorize_workspace(&workspace.to_string_lossy())?;
        let server = app.state::<crate::theia::TheiaServer>();
        let result = crate::theia::coding_theia_start(
            app.clone(),
            access,
            server,
            authorized.to_string_lossy().into_owned(),
        )
        .await;
        #[cfg(windows)]
        let frontend_result: Result<(), String> = if let Ok(endpoint) = &result {
            let started = (|| {
                // Exercise the same iframe path as the product UI. Backend readiness
                // alone does not start every Theia IPC worker or a restored terminal.
                let log_path = app
                    .path()
                    .app_data_dir()
                    .map_err(|error| error.to_string())?
                    .join("theia.log");
                let offset = std::fs::metadata(&log_path)
                    .map_err(|error| error.to_string())?
                    .len();
                let token = serde_json::to_string(&endpoint.embed_token)
                    .map_err(|error| error.to_string())?;
                let base_url =
                    serde_json::to_string(&endpoint.url).map_err(|error| error.to_string())?;
                let workspace = authorized.to_string_lossy().replace('\\', "/");
                let workspace = if workspace
                    .get(..8)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/"))
                {
                    format!("//{}", &workspace[8..])
                } else {
                    workspace
                        .strip_prefix("//?/")
                        .unwrap_or(&workspace)
                        .to_owned()
                };
                let workspace =
                    serde_json::to_string(&workspace).map_err(|error| error.to_string())?;
                let script = format!(
                    "(() => {{ const frame = document.createElement('iframe'); \
                 frame.id = 'echo-validation-ide'; \
                 frame.name = 'echo-embed:' + JSON.stringify({{ embedToken: {token}, \
                 bridgeToken: 'validation', parentOrigin: window.location.origin }}); \
                 frame.referrerPolicy = 'no-referrer'; \
                 const url = new URL({base_url}); url.hash = encodeURI({workspace}); \
                 frame.src = url.toString(); frame.style.cssText = \
                 'position:fixed;left:0;top:0;width:1200px;height:800px;opacity:0;pointer-events:none;border:0'; \
                 document.body.appendChild(frame); }})()"
                );
                window.eval(script).map_err(|error| error.to_string())?;
                Ok((log_path, offset))
            })();
            let ready = match started {
                Ok((log_path, offset)) => wait_for_theia_frontend_ready(&log_path, offset).await,
                Err(error) => Err(error),
            };
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let _ = window.eval("document.getElementById('echo-validation-ide')?.remove()");
            ready
        } else {
            Ok(())
        };
        app.state::<crate::theia::TheiaServer>().stop();
        #[cfg(windows)]
        return result.map(|_| ()).and(frontend_result);
        #[cfg(not(windows))]
        result.map(|_| ())
    }
    .await
    .err();
    #[cfg(not(any(windows, target_os = "macos")))]
    let ide_error: Option<String> = None;
    #[cfg(windows)]
    let visible_console_windows = {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        console_watch.finish()
    };
    #[cfg(not(windows))]
    let visible_console_windows: Vec<String> = Vec::new();
    crate::paths::write_private_file(
        &crate::paths::echo_agent_home_dir().join("desktop-validation.json"),
        &serde_json::to_vec(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"), "platform": std::env::consts::OS,
            "webviewRendered": true, "ipcReady": true, "resourcesPresent": true,
            "ideStarted": cfg!(any(windows, target_os = "macos")) && ide_error.is_none(),
            "ideError": ide_error,
            "cliError": cli_error,
            "visibleConsoleWindows": visible_console_windows,
        }))
        .map_err(|e| e.to_string())?,
    )?;
    crate::request_graceful_exit(app);
    Ok(())
}
