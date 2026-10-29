//! Release gate: a real WebView renders the app and completes a native IPC call.
//! Only active in a deliberately isolated validation launch.
use tauri::Manager;
static REVIEW_EVALUATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(target_os = "macos")]
static CODE_AGENT_REVIEW_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "macos")]
static LOCAL_REVIEW_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
fn code_agent_review_scope() -> Result<serde_json::Value, String> {
    use std::os::unix::fs::PermissionsExt;
    let home = crate::paths::redirected_data_home()
        .ok_or("Code Agent review requires a redirected home")?;
    let configured = std::path::PathBuf::from(
        std::env::var_os("ECHO_AGENT_HOME").ok_or("Code Agent review home is missing")?,
    );
    let metadata = std::fs::symlink_metadata(&home).map_err(|error| error.to_string())?;
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || std::fs::canonicalize(&home).map_err(|error| error.to_string())? != home
        || home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-native-review-agent-"))
        || !((home.starts_with(&temporary) && home != temporary)
            || (home.starts_with(&private_tmp) && home != private_tmp))
    {
        return Err("Code Agent review requires its canonical private temporary home".into());
    }
    Ok(serde_json::json!({
        "home": home,
        "nonce": format!("{}{}", uuid::Uuid::now_v7(), uuid::Uuid::now_v7()),
    }))
}

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

#[cfg(any(windows, target_os = "macos"))]
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
                .unwrap_or_else(|_| "echoagent-ojlab/Echo Reed".into());
            let smoke = std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_SMOKE").as_deref() == Ok("1");
            let cross_app =
                std::env::var("ECHO_DESKTOP_REVIEW_CONTROLS_CROSS_APP").as_deref() == Ok("1");
            let scope = serde_json::json!({"home":home,"cwd":cwd,"modelId":model_id,"smoke":smoke,"crossApp":cross_app});
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
        if std::env::var("ECHO_DESKTOP_REVIEW_IDE").as_deref() == Ok("1") {
            return crate::native_ide_validation::launch(window).await;
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_CORE").as_deref() == Ok("1") {
            return crate::native_core_validation::launch(&window)
                .inspect_err(|error| tracing::warn!(%error, "Core review launch rejected"));
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
        if std::env::var("ECHO_DESKTOP_REVIEW_CODE_AGENT").as_deref() == Ok("1") {
            #[cfg(target_os = "macos")]
            {
                let scope = code_agent_review_scope()?;
                if CODE_AGENT_REVIEW_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    return Ok(());
                }
                return window
                    .eval(format!(
                        "{}({scope});",
                        include_str!("../../scripts/fixtures/native-coding-agent-review.js")
                    ))
                    .map_err(|error| error.to_string());
            }
            #[cfg(not(target_os = "macos"))]
            return Err("Native Code Agent review currently requires macOS".into());
        }
        if std::env::var("ECHO_DESKTOP_REVIEW_CODE").as_deref() == Ok("1") {
            return crate::native_coding_validation::run(app, window).await;
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
        let frontend_result: Result<(), String> = if let Ok(endpoint) = &result {
            let started = (|| {
                // Exercise the same iframe path as the product UI. Backend readiness
                // alone does not start every Theia IPC worker or a restored terminal.
                let log_path = crate::theia::data_dir(&app)?.join("theia.log");
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
        result.map(|_| ()).and(frontend_result)
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
pub fn desktop_review_context(window: tauri::WebviewWindow) -> Result<serde_json::Value, String> {
    require_review_launch(&window)?;
    let phase =
        std::env::var("ECHO_DESKTOP_REVIEW_PHASE").unwrap_or_else(|_| "first-launch".into());
    if !matches!(phase.as_str(), "first-launch" | "restart") {
        return Err("Native review phase is invalid".into());
    }
    Ok(serde_json::json!({
        "phase": phase,
        "pid": std::process::id(),
        "home": crate::paths::echo_agent_home_dir(),
        "evaluations": REVIEW_EVALUATIONS.load(std::sync::atomic::Ordering::Relaxed),
        "mediaRequested": std::env::var("ECHO_DESKTOP_REVIEW_MEDIA").as_deref() == Ok("1"),
        "extendedRequested": std::env::var("ECHO_DESKTOP_REVIEW_EXTENDED").as_deref() == Ok("1"),
        "webdavRequested": std::env::var("ECHO_DESKTOP_REVIEW_WEBDAV").as_deref() == Ok("1"),
        "codeAgentRequested": std::env::var("ECHO_DESKTOP_REVIEW_CODE_AGENT").as_deref() == Ok("1"),
        "localRequested": std::env::var("ECHO_DESKTOP_REVIEW_LOCAL").as_deref() == Ok("1"),
        "coreRequested": std::env::var("ECHO_DESKTOP_REVIEW_CORE").as_deref() == Ok("1"),
        "ideRequested": std::env::var("ECHO_DESKTOP_REVIEW_IDE").as_deref() == Ok("1"),
        "browserRequested": std::env::var("ECHO_DESKTOP_REVIEW_BROWSER").as_deref() == Ok("1"),
    }))
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
