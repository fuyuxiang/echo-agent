//! Fixed lifecycle acceptance in an owned private profile and repository workspace.
use crate::native_review_scope::{ordinary_private, require_main_internal};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;

static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u8,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    nonce: String,
    phase: String,
    cycle: String,
    model_id: String,
    expected: Option<serde_json::Value>,
}

pub(crate) fn launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_internal(window)?;
    let home = crate::paths::redirected_data_home().ok_or("Missing lifecycle home")?;
    ordinary_private(&home, true)?;
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_LIFECYCLE").as_deref() != Ok("1")
        || std::env::var_os("ECHO_AGENT_HOME")
            .map(std::path::PathBuf::from)
            .as_ref()
            != Some(&home)
        || home.parent() != Some(std::path::Path::new("/private/tmp"))
        || !home.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.strip_prefix("echo-native-review-lifecycle-")
                .is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                })
        })
    {
        return Err("Lifecycle review requires its owned private temporary home".into());
    }
    let path = home.join("native-lifecycle-manifest.private.json");
    ordinary_private(&path, false)?;
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 {
        return Err("Lifecycle manifest too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Lifecycle manifest too large".into());
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|_| "Invalid lifecycle manifest")?;
    if m.schema != 1
        || m.home != home
        || m.cwd
            != std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/native-lifecycle-review")
                .join(&m.nonce)
                .join("workspace")
        || !matches!(m.phase.as_str(), "first-launch" | "restart")
        || std::env::var("ECHO_DESKTOP_REVIEW_PHASE").as_deref() != Ok(m.phase.as_str())
        || m.nonce.len() != 32
        || !m
            .nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !crate::native_review_model::allowed_model(&m.model_id)
        || !matches!(m.cycle.as_str(), "prepare" | "soak" | "final")
        || (m.phase == "restart") != (m.cycle == "final")
        || (m.cycle != "prepare")
            != m.expected
                .as_ref()
                .is_some_and(serde_json::Value::is_object)
        || (m.cycle == "prepare" && m.expected.is_some())
    {
        return Err("Unexpected lifecycle manifest scope".into());
    }
    ordinary_private(&home.join("workspaces"), true)?;
    ordinary_private(&m.cwd, true)?;
    ordinary_private(m.cwd.parent().ok_or("Missing workspace parent")?, true)?;
    crate::native_review_model::validate_config(&home, &m.model_id)?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let watcher_pid = crate::host_watchdog::start_for_native_review()?;
    let identity = serde_json::json!({"pid": std::process::id(), "watcherPid": watcher_pid,
        "restored": crate::host_watchdog::restored(), "nonce": m.nonce, "cycle": m.cycle});
    let identity_path = home.join("native-lifecycle-host.private.json");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(identity_path).map_err(|e| e.to_string())?;
        file.write_all(identity.to_string().as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let stop_home = home.clone();
    let stop_nonce = m.nonce.clone();
    let app = window.app_handle().clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let path = stop_home.join("native-lifecycle-stop.private.json");
        if ordinary_private(&path, false).is_ok() {
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.len() < 1024
                    && serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .is_some_and(|v| v["nonce"] == stop_nonce)
                {
                    crate::request_graceful_exit(app);
                    break;
                }
            }
        }
    });
    window
        .eval(format!(
            "{}\nvoid window.__echoNativeLifecycleReview({});",
            include_str!("../../scripts/fixtures/native-lifecycle-review.js"),
            serde_json::to_string(&m).map_err(|e| e.to_string())?
        ))
        .map_err(|e| e.to_string())
}
