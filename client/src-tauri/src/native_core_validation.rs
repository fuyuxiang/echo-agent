//! Fixed private core-journey acceptance. No arbitrary script or file command.
use crate::native_review_scope::{ordinary_private, require_main_internal};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfficeFixture {
    format: String,
    path: PathBuf,
    marker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    excluded_marker: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u8,
    home: PathBuf,
    cwd: PathBuf,
    nonce: String,
    phase: String,
    model_id: String,
    office_fixtures: Vec<OfficeFixture>,
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
        || !matches!(
            value.model_id.as_str(),
            "echoagent-ojlab/Echo Reed" | crate::native_review_model::PRIVATE_MODEL_ID
        )
        || value.office_fixtures.len() != 3
        || (phase == "restart")
            != value
                .expected
                .as_ref()
                .is_some_and(serde_json::Value::is_object)
        || (phase == "first-launch" && value.expected.is_some())
    {
        return Err("Core review manifest has an unexpected scope".into());
    }
    for (fixture, format) in value.office_fixtures.iter().zip(["xlsx", "epub", "ods"]) {
        if fixture.format != format
            || fixture.path != value.cwd.join(format!("办公读取/中文 {format}.{format}"))
            || fixture.marker != format!("CORE-{}-{}", format.to_uppercase(), value.nonce)
            || fixture.excluded_marker
                != (format == "epub").then(|| format!("CORE-NAV-EXCLUDED-{}", value.nonce))
        {
            return Err(
                "Core review only accepts its three fixed synthetic Office fixtures".into(),
            );
        }
    }
    Ok(())
}

fn read_manifest() -> Result<Manifest, String> {
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_CORE").as_deref() != Ok("1")
    {
        return Err("Core review is not enabled".into());
    }
    let home = crate::paths::redirected_data_home().ok_or("Core review home is not redirected")?;
    ordinary_private(&home, true)?;
    let configured = PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").ok_or("Missing core home")?);
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").map_err(|error| error.to_string())?;
    if home != configured
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("echo-native-review-core-")
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
        return Err("Core review requires its own canonical private temporary home".into());
    }
    let path = home.join("native-core-manifest.private.json");
    ordinary_private(&path, false)?;
    if std::fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len()
        > 64 * 1024
    {
        return Err("Core review manifest is too large".into());
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Core review manifest is too large".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid core review manifest")?;
    let phase = std::env::var("ECHO_DESKTOP_REVIEW_PHASE").map_err(|_| "Missing core phase")?;
    validate_manifest(&manifest, &home, &phase)?;
    crate::native_review_model::validate_config(&home, &manifest.model_id)?;
    for directory in [
        home.join("workspaces"),
        manifest.cwd.clone(),
        manifest.cwd.join("办公读取"),
    ] {
        ordinary_private(&directory, true)?;
    }
    for fixture in &manifest.office_fixtures {
        ordinary_private(&fixture.path, false)?;
        if std::fs::metadata(&fixture.path)
            .map_err(|error| error.to_string())?
            .len()
            > 1024 * 1024
        {
            return Err("Core Office fixture exceeds its fixed size limit".into());
        }
    }
    Ok(manifest)
}

#[tauri::command]
pub(crate) fn desktop_review_model_connection(
    window: tauri::WebviewWindow,
) -> Result<crate::native_review_model::ReviewConnection, String> {
    require_main_internal(&window)?;
    let manifest = read_manifest()?;
    crate::native_review_model::connection(&manifest.home, &manifest.model_id)
}

pub(crate) fn launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_internal(window)?;
    let manifest = read_manifest()?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let scope = serde_json::json!({"home": manifest.home, "nonce": manifest.nonce});
    window
        .eval(format!(
            "{}\nvoid window.__echoNativeCoreReview({scope});",
            include_str!("../../scripts/fixtures/native-core-review.js")
        ))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn desktop_review_core_manifest(
    window: tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    require_main_internal(&window)?;
    serde_json::to_value(read_manifest()?).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Manifest {
        let home = PathBuf::from("/private/tmp/echo-native-review-core-test");
        let cwd = home.join("workspaces/default");
        let nonce = "a".repeat(32);
        let office_fixtures = ["xlsx", "epub", "ods"]
            .into_iter()
            .map(|format| OfficeFixture {
                format: format.into(),
                path: cwd.join(format!("办公读取/中文 {format}.{format}")),
                marker: format!("CORE-{}-{nonce}", format.to_uppercase()),
                excluded_marker: (format == "epub").then(|| format!("CORE-NAV-EXCLUDED-{nonce}")),
            })
            .collect();
        Manifest {
            schema: 1,
            home,
            cwd,
            nonce,
            phase: "first-launch".into(),
            model_id: "echoagent-ojlab/Echo Reed".into(),
            office_fixtures,
            expected: None,
        }
    }
    #[test]
    fn core_scope_rejects_foreign_office_path_phase_model_nonce_and_expected() {
        let value = fixture();
        let home = value.home.clone();
        assert!(validate_manifest(&value, &home, "first-launch").is_ok());
        let mut value = fixture();
        value.office_fixtures[0].path = PathBuf::from("/outside/a.xlsx");
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        let mut value = fixture();
        value.model_id = "other".into();
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        let mut value = fixture();
        value.nonce = "A".repeat(32);
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        let mut value = fixture();
        value.expected = Some(serde_json::json!({"script":"arbitrary"}));
        assert!(validate_manifest(&value, &home, "first-launch").is_err());
        assert!(validate_manifest(&fixture(), &home, "restart").is_err());
    }
    #[test]
    fn core_scope_rejects_unknown_manifest_and_fixture_fields() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        assert!(serde_json::from_value::<Manifest>(value.clone()).is_ok());
        value["officeFixtures"][0]["command"] = "arbitrary".into();
        assert!(serde_json::from_value::<Manifest>(value).is_err());
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["url"] = "https://example.com".into();
        assert!(serde_json::from_value::<Manifest>(value).is_err());
    }
}
