//! Fixed private model-driven Browser Use acceptance, with a synthetic loopback page.
use crate::native_review_scope::{ordinary_private, require_main_internal};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u8,
    home: PathBuf,
    cwd: PathBuf,
    nonce: String,
    phase: String,
    model_id: String,
    url: String,
    expected: Option<serde_json::Value>,
}

fn validate_manifest(value: &Manifest, home: &Path, phase: &str) -> Result<(), String> {
    if value.schema != 1
        || value.home != home
        || value.cwd != home.join("workspaces/default")
        || value.phase != phase
        || !matches!(phase, "first-launch" | "restart")
        || value.nonce.len() != 32
        || !value
            .nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || !crate::native_review_model::allowed_model(&value.model_id)
        || (phase == "restart")
            != value
                .expected
                .as_ref()
                .is_some_and(serde_json::Value::is_object)
        || (phase == "first-launch" && value.expected.is_some())
    {
        return Err("Browser review manifest has an unexpected scope".into());
    }
    let url = url::Url::parse(&value.url).map_err(|_| "Invalid browser review URL")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.port().is_some_and(|port| port > 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != format!("/review/{}/", value.nonce)
        || url.as_str() != value.url
    {
        return Err("Browser review only accepts its exact synthetic loopback page".into());
    }
    Ok(())
}

fn read_manifest() -> Result<Manifest, String> {
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_BROWSER").as_deref() != Ok("1")
    {
        return Err("Browser review is not enabled".into());
    }
    let home =
        crate::paths::redirected_data_home().ok_or("Browser review home is not redirected")?;
    ordinary_private(&home, true)?;
    let configured =
        PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").ok_or("Missing browser home")?);
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|error| error.to_string())?;
    if home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("echo-native-review-browser-")
                    .is_some_and(|suffix| {
                        !suffix.is_empty()
                            && suffix.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
                            })
                    })
            })
        || !((home.starts_with(&temporary) && home != temporary)
            || (home.starts_with(&private_tmp) && home != private_tmp))
    {
        return Err("Browser review requires its own canonical private temporary home".into());
    }
    let path = home.join("native-browser-manifest.private.json");
    ordinary_private(&path, false)?;
    if std::fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len()
        > 64 * 1024
    {
        return Err("Browser review manifest is too large".into());
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Browser review manifest is too large".into());
    }
    let value: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid browser review manifest")?;
    let phase = std::env::var("ECHO_DESKTOP_REVIEW_PHASE").map_err(|_| "Missing browser phase")?;
    validate_manifest(&value, &home, &phase)?;
    crate::native_review_model::validate_config(&home, &value.model_id)?;
    for directory in [home.join("workspaces"), value.cwd.clone()] {
        ordinary_private(&directory, true)?;
    }
    Ok(value)
}

pub(crate) fn launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_internal(window)?;
    let value = read_manifest()?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let scope = serde_json::json!({"home": value.home, "nonce": value.nonce});
    window
        .eval(format!(
            "{}\nvoid window.__echoNativeBrowserReview({scope});",
            include_str!("../../scripts/fixtures/native-browser-review.js")
        ))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn desktop_review_browser_manifest(
    window: tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    require_main_internal(&window)?;
    serde_json::to_value(read_manifest()?).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Manifest {
        let home = PathBuf::from("/private/tmp/echo-native-review-browser-test");
        let nonce = "a".repeat(32);
        Manifest {
            schema: 1,
            cwd: home.join("workspaces/default"),
            home,
            url: format!("http://127.0.0.1:39817/review/{nonce}/"),
            nonce,
            phase: "first-launch".into(),
            model_id: "echoagent-ojlab/Echo Reed".into(),
            expected: None,
        }
    }
    #[test]
    fn browser_scope_rejects_other_urls_workspaces_models_and_phase() {
        let value = fixture();
        let home = value.home.clone();
        assert!(validate_manifest(&value, &home, "first-launch").is_ok());
        for url in [
            "https://example.com/",
            "http://localhost:39817/review/",
            "http://127.0.0.1:0/review/",
            "http://127.0.0.1/review/",
            "http://127.0.0.1:39817/other/",
        ] {
            let mut value = fixture();
            value.url = url.into();
            assert!(validate_manifest(&value, &home, "first-launch").is_err());
        }
        for suffix in ["?x=1", "#other"] {
            let mut value = fixture();
            value.url.push_str(suffix);
            assert!(validate_manifest(&value, &home, "first-launch").is_err());
        }
        let mut value = fixture();
        value.cwd = PathBuf::from("/outside");
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        let mut value = fixture();
        value.model_id = "echoagent-ojlab/Echo Grove".into();
        assert!(validate_manifest(&value, &home, "first-launch").is_ok());
        let mut value = fixture();
        value.model_id = "other".into();
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        assert!(validate_manifest(&fixture(), &home, "restart").is_err());
    }
    #[test]
    fn browser_scope_rejects_unknown_script_field_and_accepts_only_restart_object() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["script"] = "arbitrary".into();
        assert!(serde_json::from_value::<Manifest>(value).is_err());
        let mut value = fixture();
        let home = value.home.clone();
        value.phase = "restart".into();
        value.expected = Some(serde_json::json!({"ownedSessionId":"test"}));
        assert!(validate_manifest(&value, &home, "restart").is_ok());
        value.expected = Some(serde_json::json!([]));
        assert!(validate_manifest(&value, &home, "restart").is_err());
    }
}
