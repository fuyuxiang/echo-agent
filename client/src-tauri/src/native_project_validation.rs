//! Fixed project journey against an explicitly selected real organization.
//! Uses production DOM/IPC and model calls; no arbitrary fixture or command.
use crate::native_review_scope::{ordinary_private, require_main_internal};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u8,
    home: PathBuf,
    cwd: PathBuf,
    nonce: String,
    phase: String,
    model_id: String,
    server_url: String,
    expected_user_id: String,
    expected_username: String,
    expected: Option<serde_json::Value>,
}

fn validate(m: &Manifest, home: &Path, profile: &serde_json::Value) -> Result<(), String> {
    let url = url::Url::parse(&m.server_url).map_err(|_| "Invalid organization origin")?;
    if m.schema != 1
        || m.home != home
        || m.cwd != home.join("workspaces/default")
        || home.parent() != Some(Path::new("/private/tmp"))
        || !home.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
            s.strip_prefix("echo-native-review-project-")
                .is_some_and(|v| {
                    !v.is_empty()
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                })
        })
        || !matches!(m.phase.as_str(), "first-launch" | "restart")
        || (m.phase == "restart")
            != m.expected
                .as_ref()
                .is_some_and(serde_json::Value::is_object)
        || (m.phase == "first-launch" && m.expected.is_some())
        || m.nonce.len() != 32
        || !m
            .nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || m.model_id != "organization/MiniMax-M3.1-Flash"
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || m.expected_user_id.is_empty()
        || m.expected_username.is_empty()
        || profile["serverUrl"].as_str() != Some(m.server_url.as_str())
        || profile["userId"].as_str() != Some(m.expected_user_id.as_str())
        || profile["username"].as_str() != Some(m.expected_username.as_str())
    {
        return Err("Project review manifest differs from its private account/model scope".into());
    }
    Ok(())
}

pub(crate) fn launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_internal(window)?;
    let home = crate::paths::redirected_data_home().ok_or("Missing project review home")?;
    ordinary_private(&home, true)?;
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_PROJECT").as_deref() != Ok("1")
        || std::env::var_os("ECHO_AGENT_HOME")
            .map(PathBuf::from)
            .as_ref()
            != Some(&home)
    {
        return Err("Project review requires its explicit isolated launch".into());
    }
    let read = |name: &str| -> Result<serde_json::Value, String> {
        let path = home.join(name);
        ordinary_private(&path, false)?;
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 {
            return Err("Project review manifest exceeds its bound".into());
        }
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "Invalid private project manifest".into())
    };
    let m: Manifest = serde_json::from_value(read("native-project-manifest.private.json")?)
        .map_err(|_| "Invalid private project manifest fields")?;
    validate(&m, &home, &read("organization-profile.json")?)?;
    if std::env::var("ECHO_DESKTOP_REVIEW_PHASE").as_deref() != Ok(m.phase.as_str()) {
        return Err("Project review phase differs".into());
    }
    ordinary_private(&home.join("workspaces"), true)?;
    ordinary_private(&m.cwd, true)?;
    // A strict private bootstrap model initializes the isolated runtime. Actual
    // project turns use only the pinned model restored from this real account.
    crate::native_review_model::validate_config(
        &home,
        crate::native_review_model::PRIVATE_MODEL_ID,
    )?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    window
        .eval(format!(
            "{}\nvoid window.__echoNativeProjectReview({});",
            include_str!("../../scripts/fixtures/native-project-review.js"),
            serde_json::to_string(&m).map_err(|e| e.to_string())?
        ))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Manifest, serde_json::Value) {
        let home = PathBuf::from("/private/tmp/echo-native-review-project-test");
        let m = Manifest {
            schema: 1,
            cwd: home.join("workspaces/default"),
            home,
            nonce: "0123456789abcdef0123456789abcdef".into(),
            phase: "first-launch".into(),
            model_id: "organization/MiniMax-M3.1-Flash".into(),
            server_url: "https://org.example:8787".into(),
            expected_user_id: "real-user".into(),
            expected_username: "reviewer".into(),
            expected: None,
        };
        let profile = serde_json::json!({"serverUrl":m.server_url,"userId":m.expected_user_id,"username":m.expected_username});
        (m, profile)
    }
    #[test]
    fn project_review_pins_the_real_account_and_isolated_workspace() {
        let (m, profile) = fixture();
        assert!(validate(&m, &m.home, &profile).is_ok());
        for field in ["serverUrl", "userId", "username"] {
            let mut wrong = profile.clone();
            wrong[field] = serde_json::json!("foreign");
            assert!(validate(&m, &m.home, &wrong).is_err());
        }
        let mut wrong = m.clone();
        wrong.cwd = PathBuf::from("/tmp/foreign");
        assert!(validate(&wrong, &m.home, &profile).is_err());
        wrong = m.clone();
        wrong.model_id = "arbitrary-model".into();
        assert!(validate(&wrong, &m.home, &profile).is_err());
    }
    #[test]
    fn project_review_rejects_untrusted_origins_and_unbound_restart() {
        let (m, profile) = fixture();
        for origin in [
            "http://org.example",
            "https://secret@org.example",
            "https://org.example?key=x",
            "https://org.example/path",
        ] {
            let mut wrong = m.clone();
            wrong.server_url = origin.into();
            let mut p = profile.clone();
            p["serverUrl"] = serde_json::json!(origin);
            assert!(validate(&wrong, &m.home, &p).is_err());
        }
        let mut restart = m.clone();
        restart.phase = "restart".into();
        assert!(validate(&restart, &m.home, &profile).is_err());
        restart.expected = Some(serde_json::json!({"projectId":"owned"}));
        assert!(validate(&restart, &m.home, &profile).is_ok());
    }
}
