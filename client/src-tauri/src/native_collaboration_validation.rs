//! Fixed native group/scheduler journey, confined to a private home.
use crate::native_review_scope::{ordinary_private, require_main_internal};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema: u8,
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    nonce: String,
    phase: String,
    model_id: String,
    expected: Option<serde_json::Value>,
}

pub(crate) fn launch(window: &tauri::WebviewWindow) -> Result<(), String> {
    require_main_internal(window)?;
    let home = crate::paths::redirected_data_home().ok_or("Missing collaboration home")?;
    ordinary_private(&home, true)?;
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_COLLABORATION").as_deref() != Ok("1")
        || std::env::var_os("ECHO_AGENT_HOME")
            .map(std::path::PathBuf::from)
            .as_ref()
            != Some(&home)
        || home.parent() != Some(std::path::Path::new("/private/tmp"))
        || !home.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.strip_prefix("echo-native-review-collaboration-")
                .is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                })
        })
    {
        return Err("Collaboration review requires its owned private temporary home".into());
    }
    let path = home.join("native-collaboration-manifest.private.json");
    ordinary_private(&path, false)?;
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 {
        return Err("Collaboration manifest too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Collaboration manifest too large".into());
    }
    let m: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid collaboration manifest")?;
    if m.schema != 1
        || m.home != home
        || m.cwd != home.join("workspaces/default")
        || !matches!(m.phase.as_str(), "first-launch" | "restart")
        || std::env::var("ECHO_DESKTOP_REVIEW_PHASE").as_deref() != Ok(m.phase.as_str())
        || m.nonce.len() != 32
        || !m
            .nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || !crate::native_review_model::allowed_model(&m.model_id)
        || (m.phase == "restart")
            != m.expected
                .as_ref()
                .is_some_and(serde_json::Value::is_object)
        || (m.phase == "first-launch" && m.expected.is_some())
    {
        return Err("Unexpected collaboration manifest scope".into());
    }
    ordinary_private(&home.join("workspaces"), true)?;
    ordinary_private(&m.cwd, true)?;
    crate::native_review_model::validate_config(&home, &m.model_id)?;
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    window
        .eval(format!(
            "{}\nvoid window.__echoNativeCollaborationReview({});",
            include_str!("../../scripts/fixtures/native-collaboration-review.js"),
            serde_json::to_string(&m).map_err(|e| e.to_string())?
        ))
        .map_err(|e| e.to_string())
}
