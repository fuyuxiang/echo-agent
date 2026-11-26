//! Fixed, private native lifecycle review. No arbitrary script, URL or home.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tauri::Manager;

static STARTED: AtomicBool = AtomicBool::new(false);
static EVALUATIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Scope {
    schema: u8,
    home: PathBuf,
    cwd: PathBuf,
    nonce: String,
    phase: String,
    expected_session_id: Option<String>,
}

fn enabled() -> bool {
    std::env::var("ECHO_DESKTOP_REVIEW").as_deref() == Ok("1")
        && std::env::var("ECHO_DESKTOP_REVIEW_EXIT").as_deref() == Ok("1")
}

fn internal_url(url: &url::Url) -> bool {
    matches!(url.scheme(), "tauri" | "http" | "https")
        && matches!(url.host_str(), Some("localhost" | "tauri.localhost"))
        && (url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/" | "/index.html")
}

fn ordinary_private(path: &Path, directory: bool) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink()
        || metadata.is_dir() != directory
        || (!directory && !metadata.is_file())
        || std::fs::canonicalize(path).map_err(|error| error.to_string())? != path
    {
        return Err("Exit review requires canonical ordinary private paths".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Exit review path is not private".into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("Exit review currently requires Unix private permissions".into())
    }
}

fn validate_scope(scope: &Scope, home: &Path, phase: &str) -> Result<(), String> {
    if scope.schema != 1
        || scope.home != home
        || scope.cwd != home.join("workspaces/default")
        || scope.phase != phase
        || !matches!(phase, "init-failure" | "flush-failure" | "restart")
        || scope.nonce.len() != 32
        || !scope
            .nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || scope.expected_session_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        || (phase == "restart") != scope.expected_session_id.is_some()
    {
        return Err("Exit review manifest does not match its fixed scope".into());
    }
    Ok(())
}

fn read_scope() -> Result<Scope, String> {
    if !enabled() {
        return Err("Exit review is not enabled".into());
    }
    let home = crate::paths::redirected_data_home().ok_or("Exit review home is not redirected")?;
    let configured = PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").ok_or("Missing exit home")?);
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|error| error.to_string())?;
    ordinary_private(&home, true)?;
    if home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-native-review-exit-"))
        || !((home.starts_with(&temporary) && home != temporary)
            || (home.starts_with(&private_tmp) && home != private_tmp))
    {
        return Err("Exit review requires its own nested temporary home".into());
    }
    let manifest = PathBuf::from(
        std::env::var_os("ECHO_EXIT_REVIEW_MANIFEST").ok_or("Missing exit manifest")?,
    );
    if manifest != home.join("native-exit-manifest.private.json") {
        return Err("Exit review manifest is outside its fixed home".into());
    }
    ordinary_private(&manifest, false)?;
    if std::fs::metadata(&manifest)
        .map_err(|error| error.to_string())?
        .len()
        > 8192
    {
        return Err("Exit review manifest is too large".into());
    }
    let bytes = std::fs::read(&manifest).map_err(|error| error.to_string())?;
    if bytes.len() > 8192 {
        return Err("Exit review manifest is too large".into());
    }
    let scope: Scope =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid exit review manifest")?;
    let phase = std::env::var("ECHO_EXIT_REVIEW_PHASE").map_err(|_| "Missing exit phase")?;
    validate_scope(&scope, &home, &phase)?;
    ordinary_private(&scope.cwd, true)?;
    Ok(scope)
}

pub(crate) fn on_page_load(
    webview: &tauri::Webview,
    payload: &tauri::webview::PageLoadPayload<'_>,
) -> Result<(), String> {
    if !enabled() || payload.event() != tauri::webview::PageLoadEvent::Finished {
        return Ok(());
    }
    if webview.label() != "main" || !internal_url(payload.url()) {
        return Err("Exit review requires the main internal application page".into());
    }
    read_scope()?;
    if STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    // Entire executable body is embedded at compile time, not read from disk.
    let script = concat!(
        include_str!("../../scripts/fixtures/native-exit-recovery-review.js"),
        "\nvoid window.__echoNativeExitRecoveryReview();"
    );
    webview.eval(script).map_err(|error| error.to_string())?;
    EVALUATIONS.fetch_add(1, Ordering::Release);
    Ok(())
}

#[tauri::command]
pub fn native_exit_review_context(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    if window.label() != "main" || !internal_url(&window.url().map_err(|error| error.to_string())?)
    {
        return Err("Exit review context requires the main internal page".into());
    }
    let scope = read_scope()?;
    let state = app
        .try_state::<crate::harness::state::HarnessState>()
        .ok_or("Exit review native state is not ready")?;
    let (has_runtime_handle, shutdown_failure) = {
        let handle = state
            .handle
            .lock()
            .map_err(|_| "Exit review handle lock failed")?;
        (
            handle.is_some(),
            handle.as_ref().and_then(|handle| {
                handle
                    .shutdown_failure
                    .lock()
                    .ok()
                    .and_then(|reason| reason.clone())
            }),
        )
    };
    let has_runtime_sender = state
        .tx
        .lock()
        .map_err(|_| "Exit review sender lock failed")?
        .is_some();
    let has_init_outcome = state
        .init_outcome
        .lock()
        .map_err(|_| "Exit review init lock failed")?
        .is_some();
    Ok(serde_json::json!({
        "scope": scope,
        "pid": std::process::id(),
        "evaluations": EVALUATIONS.load(Ordering::Acquire),
        "lifecycle": crate::exit_review_lifecycle_snapshot(),
        "hasRuntimeHandle": has_runtime_handle,
        "hasRuntimeSender": has_runtime_sender,
        "hasInitOutcome": has_init_outcome,
        "initGeneration": state.init_generation.load(Ordering::Acquire),
        "shutdownFailure": shutdown_failure,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            schema: 1,
            home: PathBuf::from("/private/tmp/echo-native-review-exit-test"),
            cwd: PathBuf::from("/private/tmp/echo-native-review-exit-test/workspaces/default"),
            nonce: "a".repeat(32),
            phase: "flush-failure".into(),
            expected_session_id: None,
        }
    }

    #[test]
    fn exit_scope_rejects_wrong_home_workspace_phase_nonce_and_session() {
        let mut value = scope();
        let home = value.home.clone();
        assert!(validate_scope(&value, &home, "flush-failure").is_ok());
        value.cwd = home.join("../outside");
        assert!(validate_scope(&value, &home, "flush-failure").is_err());
        value = scope();
        value.home = PathBuf::from("/outside");
        assert!(validate_scope(&value, &home, "flush-failure").is_err());
        value = scope();
        value.nonce = "A".repeat(32);
        assert!(validate_scope(&value, &home, "flush-failure").is_err());
        value = scope();
        value.expected_session_id = Some("../foreign".into());
        assert!(validate_scope(&value, &home, "restart").is_err());
        value = scope();
        assert!(validate_scope(&value, &home, "restart").is_err());
    }

    #[test]
    fn exit_scope_rejects_unknown_manifest_fields_and_external_urls() {
        let mut value = serde_json::to_value(scope()).unwrap();
        value["script"] = "arbitrary".into();
        assert!(serde_json::from_value::<Scope>(value).is_err());
        for value in [
            "https://example.com/",
            "http://127.0.0.1:1420/",
            "tauri://localhost/?script=1",
            "tauri://localhost/other",
        ] {
            assert!(!internal_url(&url::Url::parse(value).unwrap()));
        }
        assert!(internal_url(
            &url::Url::parse("tauri://localhost/index.html").unwrap()
        ));
    }
}
