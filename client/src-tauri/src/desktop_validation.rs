//! Release gate: a real WebView renders the app and completes a native IPC call.
//! Only active in a deliberately isolated validation launch.
use tauri::Manager;
static REVIEW_EVALUATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(target_os = "macos")]
static LOCAL_REVIEW_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
fn webdav_review_scope() -> Result<serde_json::Value, String> {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    let ordinary_private = |path: &Path, directory: bool| -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink()
            || metadata.is_dir() != directory
            || (!directory && !metadata.is_file())
            || metadata.permissions().mode() & 0o077 != 0
            || std::fs::canonicalize(path).map_err(|error| error.to_string())? != path
        {
            return Err(
                "WebDAV review requires canonical private ordinary files/directories".into(),
            );
        }
        Ok(())
    };
    let home =
        crate::paths::redirected_data_home().ok_or("WebDAV review requires a redirected home")?;
    ordinary_private(&home, true)?;
    let configured =
        PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").ok_or("WebDAV review home is missing")?);
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|error| error.to_string())?;
    if home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-native-review-"))
        || !((home.starts_with(&temporary) && home != temporary)
            || (home.starts_with(&private_tmp) && home != private_tmp))
    {
        return Err("WebDAV review may only use its nested temporary review home".into());
    }
    let manifest = PathBuf::from(
        std::env::var_os("ECHO_WEBDAV_REVIEW_MANIFEST")
            .ok_or("WebDAV review manifest is missing")?,
    );
    if manifest != home.join("native-webdav-manifest.private.json") {
        return Err("WebDAV review manifest is outside its dedicated home".into());
    }
    ordinary_private(&manifest, false)?;
    if std::fs::metadata(&manifest)
        .map_err(|error| error.to_string())?
        .len()
        > 32 * 1024
    {
        return Err("WebDAV review manifest exceeds the fixture limit".into());
    }
    let bytes = std::fs::read(&manifest).map_err(|error| error.to_string())?;
    if bytes.len() > 32 * 1024 {
        return Err("WebDAV review manifest exceeds the fixture limit".into());
    }
    let scope: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid WebDAV review manifest")?;
    let text = |key: &str| {
        scope[key]
            .as_str()
            .ok_or_else(|| format!("WebDAV review field {key} is missing"))
    };
    if scope["schema"] != 1
        || scope["readOnlySharesPhysicalRoot"] != true
        || Path::new(text("home")?) != home
    {
        return Err("WebDAV review manifest has a different scope".into());
    }
    let root = PathBuf::from(text("root")?);
    let run = root
        .parent()
        .ok_or("WebDAV review server root has no parent")?;
    if root.file_name().and_then(|name| name.to_str()) != Some("physical-root")
        || run.parent() != Some(private_tmp.as_path())
        || !run
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-webdav-real-"))
        || Path::new(text("requestAudit")?) != run.join("requests.redacted.jsonl")
    {
        return Err("WebDAV review server is outside its dedicated temporary scope".into());
    }
    ordinary_private(run, true)?;
    ordinary_private(&root, true)?;
    for share in ["rw", "ro"] {
        let path = root.join(share);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || std::fs::canonicalize(&path).map_err(|error| error.to_string())? != path
        {
            return Err("WebDAV review shares must be ordinary scoped directories".into());
        }
    }
    let rw = url::Url::parse(text("readWriteUrl")?).map_err(|_| "Invalid WebDAV review URL")?;
    let ro = url::Url::parse(text("readOnlyUrl")?).map_err(|_| "Invalid WebDAV review URL")?;
    for (endpoint, path) in [(&rw, "/rw"), (&ro, "/ro")] {
        if endpoint.scheme() != "http"
            || endpoint.host_str() != Some("127.0.0.1")
            || endpoint.port().is_none()
            || endpoint.path() != path
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err("WebDAV review only accepts its credential-free loopback endpoints".into());
        }
    }
    let nonce = text("nonce")?;
    if rw.origin() != ro.origin()
        || nonce.len() != 32
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || text("username")?.is_empty()
        || text("username")?.len() > 128
        || text("password")?.len() < 20
        || text("password")?.len() > 256
        || text("seedName")? != "原始 中文.txt"
        || text("seedContent")?.is_empty()
        || text("seedContent")?.len() > 4096
    {
        return Err("WebDAV review fixture fields are invalid".into());
    }
    Ok(scope)
}

#[cfg(target_os = "macos")]
fn local_review_scope() -> Result<serde_json::Value, String> {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    let ordinary_private = |path: &Path, directory: bool| -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink()
            || metadata.is_dir() != directory
            || (!directory && !metadata.is_file())
            || metadata.permissions().mode() & 0o077 != 0
            || std::fs::canonicalize(path).map_err(|error| error.to_string())? != path
        {
            return Err("Local review requires canonical private ordinary paths".into());
        }
        Ok(())
    };
    let home = crate::paths::redirected_data_home().ok_or("Local review home is missing")?;
    ordinary_private(&home, true)?;
    let configured =
        PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").ok_or("Local review home is missing")?);
    let temporary = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|e| e.to_string())?;
    if home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-native-review-local-"))
        || !((home.starts_with(&temporary) && home != temporary)
            || (home.starts_with(&private_tmp) && home != private_tmp))
    {
        return Err("Local review requires its dedicated private temporary home".into());
    }
    let manifest = PathBuf::from(
        std::env::var_os("ECHO_LOCAL_REVIEW_MANIFEST").ok_or("Local review manifest is missing")?,
    );
    if manifest != home.join("native-local-manifest.private.json") {
        return Err("Local review manifest must be inside its dedicated home".into());
    }
    ordinary_private(&manifest, false)?;
    if std::fs::metadata(&manifest)
        .map_err(|e| e.to_string())?
        .len()
        > 32 * 1024
    {
        return Err("Local review manifest exceeds the fixture limit".into());
    }
    let bytes = std::fs::read(&manifest).map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 {
        return Err("Local review manifest exceeds the fixture limit".into());
    }
    let scope: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid local review manifest")?;
    let nonce = scope["nonce"]
        .as_str()
        .ok_or("Local review nonce is missing")?;
    if scope["schema"] != 1
        || scope["home"].as_str().map(Path::new) != Some(home.as_path())
        || !(24..=128).contains(&nonce.len())
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Local review manifest has an invalid scope".into());
    }
    let receiver = scope["receiverUrl"]
        .as_str()
        .ok_or("Local receiver URL is missing")?;
    let endpoint = url::Url::parse(receiver).map_err(|_| "Invalid local receiver URL")?;
    if endpoint.scheme() != "http"
        || endpoint.host_str() != Some("127.0.0.1")
        || !endpoint.port().is_some_and(|port| port > 0)
        || endpoint.path() != format!("/echo-review/{nonce}")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err("Local review only accepts its fixed credential-free loopback receiver".into());
    }
    Ok(serde_json::json!({ "schema": 1, "home": home, "nonce": nonce, "receiverUrl": receiver }))
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

    type ProcessMap = HashMap<u32, (u32, String)>;

    fn app_process_tree() -> (HashSet<u32>, ProcessMap) {
        let root = std::process::id();
        let mut owned = HashSet::from([root]);
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return (owned, HashMap::new());
        }
        let mut parents = HashMap::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while found {
            let length = entry
                .szExeFile
                .iter()
                .position(|&value| value == 0)
                .unwrap_or(entry.szExeFile.len());
            parents.insert(
                entry.th32ProcessID,
                (
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..length]),
                ),
            );
            found = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot) };
        loop {
            let before = owned.len();
            for (&pid, (parent, _)) in &parents {
                if owned.contains(parent) {
                    owned.insert(pid);
                }
            }
            if owned.len() == before {
                return (owned, parents);
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
        phase: Arc<Mutex<&'static str>>,
        thread: thread::JoinHandle<()>,
    }

    impl Watch {
        pub(super) fn start() -> Self {
            let baseline: HashSet<_> = visible().into_keys().collect();
            let stop = Arc::new(AtomicBool::new(false));
            let observed = Arc::new(Mutex::new(HashSet::new()));
            let phase = Arc::new(Mutex::new("cli"));
            let stop_in_thread = stop.clone();
            let observed_in_thread = observed.clone();
            let phase_in_thread = phase.clone();
            let thread = thread::spawn(move || {
                let mut inspected = baseline;
                while !stop_in_thread.load(Ordering::Relaxed) {
                    let new_windows: Vec<_> = visible()
                        .into_iter()
                        .filter(|((handle, pid), _)| inspected.insert((*handle, *pid)))
                        .collect();
                    if !new_windows.is_empty() {
                        let (owned, processes) = app_process_tree();
                        for ((handle, pid), class) in new_windows {
                            if !owned.contains(&pid) {
                                continue;
                            }
                            if let Ok(mut seen) = observed_in_thread.lock() {
                                let mut chain = Vec::new();
                                let mut current = pid;
                                for _ in 0..16 {
                                    let Some((parent, executable)) = processes.get(&current) else {
                                        break;
                                    };
                                    chain.push(format!("{executable}[{current}]"));
                                    if current == std::process::id() || *parent == current {
                                        break;
                                    }
                                    current = *parent;
                                }
                                let phase = phase_in_thread
                                    .lock()
                                    .map(|phase| *phase)
                                    .unwrap_or("unknown");
                                seen.insert(format!(
                                    "{class} PID={pid} HWND={handle} phase={phase} processTree={}",
                                    chain.join(" <- ")
                                ));
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
            Self {
                stop,
                observed,
                phase,
                thread,
            }
        }

        pub(super) fn set_phase(&self, value: &'static str) {
            if let Ok(mut phase) = self.phase.lock() {
                *phase = value;
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
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() == Ok("1") {
        tracing::info!("Native review renderer requested dispatch");
        require_review_launch(&window)?;
        if std::env::var("ECHO_DESKTOP_REVIEW_EXIT").as_deref() == Ok("1") {
            return Ok(());
        }
        REVIEW_EVALUATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS").as_deref() == Ok("1") {
            let home = crate::paths::echo_agent_home_dir();
            crate::native_review_scope::ordinary_private(&home, true)?;
            if !home.starts_with("/private/tmp")
                || !home
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("echo-native-review-controls-"))
            {
                return Err(
                    "Desktop control review requires a private temporary controls home".into(),
                );
            }
            let cwd = crate::paths::default_workspace_dir()?;
            if !cwd.starts_with(&home) {
                return Err("Desktop control review workspace escaped home".into());
            }
            let model_id = std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_MODEL_ID")
                .unwrap_or_else(|_| "echoagent-review/claude-opus-5-5".into());
            let smoke = std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_SMOKE").as_deref() == Ok("1");
            let cross_app =
                std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_CROSS_APP").as_deref() == Ok("1");
            let scope = serde_json::json!({"home":home,"cwd":cwd,"modelId":model_id,"smoke":smoke,"crossApp":cross_app});
            if std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_WECHAT").as_deref() == Ok("1") {
                if std::env::var("ECHO_DESKTOP_REVIEW_WECHAT_SEND").as_deref() != Ok("1") {
                    return Err(
                        "Real WeChat acceptance requires explicit send authorization".into(),
                    );
                }
                return window
                    .eval(format!(
                        "{}\nwindow.__echoDesktopWechatFlow({scope});",
                        include_str!("../../scripts/fixtures/native-desktop-wechat-flow.js")
                    ))
                    .map_err(|e| e.to_string());
            }
            if std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_OPAQUE").as_deref() == Ok("1") {
                return window
                    .eval(format!(
                        "{}\nwindow.__echoDesktopVisualFlow({scope});",
                        include_str!("../../scripts/fixtures/native-desktop-visual-flow.js")
                    ))
                    .map_err(|e| e.to_string());
            }
            if std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_TASK_FLOW").as_deref() == Ok("1") {
                return window
                    .eval(format!(
                        "{}\nwindow.__echoDesktopTaskFlow({scope});",
                        include_str!("../../scripts/fixtures/native-desktop-task-flow.js")
                    ))
                    .map_err(|e| e.to_string());
            }
            if std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_JOURNEY").as_deref() == Ok("1") {
                return window
                    .eval(format!(
                        "{}\nwindow.__echoDesktopControlsJourney({scope});",
                        include_str!("../../scripts/fixtures/native-desktop-controls-journey.js")
                    ))
                    .map_err(|e| e.to_string());
            }
            return window
                .eval(format!(
                    "{}\nwindow.__echoDesktopControlsReview({scope});",
                    include_str!("../../scripts/fixtures/native-desktop-controls-review.js")
                ))
                .map_err(|e| e.to_string());
        }

        if std::env::var("ECHO_DESKTOP_REVIEW_BROWSER").as_deref() == Ok("1") {
            return crate::native_browser_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_PROJECT").as_deref() == Ok("1") {
            return crate::native_project_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_CORE").as_deref() == Ok("1") {
            return crate::native_core_validation::launch(&window)
                .inspect_err(|error| tracing::warn!(%error, "Core review launch rejected"));
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_COLLABORATION").as_deref() == Ok("1") {
            return crate::native_collaboration_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_EXTENSIONS").as_deref() == Ok("1") {
            return crate::native_extensions_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_KNOWLEDGE").as_deref() == Ok("1") {
            return crate::native_knowledge_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_LIFECYCLE").as_deref() == Ok("1") {
            return crate::native_lifecycle_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_MEMORYAUTO").as_deref() == Ok("1") {
            return crate::native_memoryauto_validation::launch(&window);
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_LOCAL").as_deref() == Ok("1") {
            #[cfg(target_os = "macos")]
            {
                let scope = local_review_scope()?;
                if LOCAL_REVIEW_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    return Ok(());
                }
                return window
                    .eval(format!(
                        "{}\nwindow.__echoNativeLocalReviewLaunch({scope});",
                        include_str!("../../scripts/fixtures/native-local-review.js")
                    ))
                    .map_err(|error| error.to_string());
            }
            #[cfg(not(target_os = "macos"))]
            return Err("Native local UI review currently requires macOS".into());
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_WEBDAV").as_deref() == Ok("1") {
            #[cfg(target_os = "macos")]
            {
                let scope = webdav_review_scope()?;
                return window
                    .eval(format!(
                        "{}\nwindow.__echoNativeWebDavReviewLaunch({scope});",
                        include_str!("../../scripts/fixtures/native-webdav-review.js")
                    ))
                    .map_err(|error| error.to_string());
            }
            #[cfg(not(target_os = "macos"))]
            return Err("Native WebDAV UI review currently requires macOS".into());
        }
        let review_script = format!(
            "{}\n{}\n{}\n{}",
            include_str!("../../scripts/fixtures/native-knowledge-review.js"),
            include_str!("../../scripts/fixtures/native-automation-review.js"),
            include_str!("../../scripts/fixtures/native-plan-review.js"),
            include_str!("../../scripts/fixtures/native-desktop-review.js"),
        );
        window
            .eval(&review_script)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    if std::env::var("ECHO_DESKTOP_VALIDATION").as_deref() != Ok("1") {
        return Ok(());
    }
    if window.label() != "main" || std::env::var_os("ECHO_AGENT_HOME").is_none() {
        return Err("Validation requires an isolated runtime home and the main window".into());
    }
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
    for relative in [
        if cfg!(windows) {
            "office/node/node.exe"
        } else {
            "office/node/bin/node"
        },
        "office/node/LICENSE",
        "office/worker.mjs",
        "office/fonts/NotoSansCJKsc-Regular.otf",
    ] {
        if !resources.join(relative).is_file() {
            return Err(format!("Packaged resource missing: {relative}"));
        }
    }
    let retired_payload = if cfg!(windows) {
        [
            "theia/parent-guard.cjs",
            "theia/browser/lib/backend/main.js",
            "theia/node/node.exe",
        ]
        .iter()
        .any(|file| resources.join(file).exists())
    } else {
        resources.join("theia").exists()
    };
    if retired_payload {
        return Err("Retired IDE resources found in the application package".into());
    }
    #[cfg(windows)]
    let console_watch = console_windows::Watch::start();
    #[cfg(windows)]
    console_watch.set_phase("office-export");
    let office = async {
        let directory = tempfile::Builder::new()
            .prefix("办公验证 ")
            .tempdir_in(crate::paths::echo_agent_home_dir())
            .map_err(|error| error.to_string())?;
        let output_root = if cfg!(windows)
            && std::env::var_os("ECHO_VALIDATE_LONG_PATHS").as_deref()
                == Some(std::ffi::OsStr::new("1"))
        {
            let path = directory
                .path()
                .join("长路径办公验证".repeat(18))
                .join("文件生成".repeat(32));
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            path.canonicalize().map_err(|error| error.to_string())?
        } else {
            directory.path().to_owned()
        };
        for format in ["docx", "xlsx", "pptx", "pdf"] {
            let output = output_root.join(format!("导出 成果.{format}"));
            let request = crate::document_export::DocumentExportRequest {
                title: "桌面办公验证".into(),
                markdown: "# 测试文档\n\n| 项目 | 金额 |\n| --- | --- |\n| 示例 | 100 |".into(),
                format: format.into(),
                workbook: None,
                slides: None,
                document: None,
            };
            crate::document_export::generate(&app, &request, &output).await?;
            crate::document_export::validate_output(&output, format)?;
        }
        Ok::<(), String>(())
    }
    .await;
    #[cfg(windows)]
    let visible_console_windows = {
        console_watch.set_phase("post-shutdown");
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
            "officeExported": office.is_ok(),
            "officeError": office.err(),
            "visibleConsoleWindows": visible_console_windows,
        }))
        .map_err(|e| e.to_string())?,
    )?;
    crate::request_graceful_exit(app);
    Ok(())
}

fn require_review_launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || window.label() != "main"
        || std::env::var_os("ECHO_AGENT_HOME").is_none()
        || crate::paths::redirected_data_home().is_none()
    {
        return Err("Native review requires an isolated home and main window".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_review_context(
    window: tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    require_review_launch(&window)?;
    let phase =
        std::env::var("ECHO_DESKTOP_REVIEW_PHASE").unwrap_or_else(|_| "first-launch".into());
    if !matches!(phase.as_str(), "first-launch" | "restart") {
        return Err("Native review phase is invalid".into());
    }
    let context = serde_json::json!({
        "phase": phase,
        "pid": std::process::id(),
        "home": crate::paths::echo_agent_home_dir(),
        "evaluations": REVIEW_EVALUATIONS.load(std::sync::atomic::Ordering::Relaxed),
        "mediaRequested": std::env::var("ECHO_DESKTOP_REVIEW_MEDIA").as_deref() == Ok("1"),
        "extendedRequested": std::env::var("ECHO_DESKTOP_REVIEW_EXTENDED").as_deref() == Ok("1"),
        "webdavRequested": std::env::var("ECHO_DESKTOP_REVIEW_WEBDAV").as_deref() == Ok("1"),
        "localRequested": std::env::var("ECHO_DESKTOP_REVIEW_LOCAL").as_deref() == Ok("1"),
        "coreRequested": std::env::var("ECHO_DESKTOP_REVIEW_CORE").as_deref() == Ok("1"),
        "browserRequested": std::env::var("ECHO_DESKTOP_REVIEW_BROWSER").as_deref() == Ok("1"),
    });
    Ok(context)
}

#[tauri::command]
pub async fn desktop_review_complete(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    report: serde_json::Value,
) -> Result<(), String> {
    require_review_launch(&window)?;
    let encoded = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    if encoded.len() > 128 * 1024 {
        return Err("Native review report too large".into());
    }
    crate::paths::write_private_file(
        &crate::paths::echo_agent_home_dir().join("native-review.json"),
        &encoded,
    )?;
    // Development reloads and repeated ready events must not overwrite the
    // original failure proof. Each native launch retains all renderer reports.
    use std::io::Write;
    let mut history = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::paths::echo_agent_home_dir().join("native-review-history.jsonl"))
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut history, &report).map_err(|error| error.to_string())?;
    writeln!(history).map_err(|error| error.to_string())?;
    crate::request_graceful_exit(app);
    Ok(())
}
