//! Signed desktop application updates served from the EchoAgent intranet.
//!
//! The official Tauri updater performs platform installation and mandatory
//! minisign verification. EchoAgent wraps its Rust API instead of exposing the
//! default JavaScript plugin commands so every request uses the organization
//! CA bundled with the application.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

const UPDATE_CA_PEM: &[u8] = include_bytes!("../certs/echo-agent-server-ca.pem");
const UPDATE_TIMEOUT: Duration = Duration::from_secs(12);
const UPDATE_PROGRESS_EVENT: &str = "app-update-progress";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateCheck {
    current_version: String,
    checked_at: String,
    update: Option<AppUpdateInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    version: String,
    notes: Option<String>,
    published_at: Option<String>,
    mandatory: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdateProgress {
    event: &'static str,
    downloaded: u64,
    total: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppUpdateErrorStage {
    Check,
    Download,
    Install,
}

#[derive(Debug, Serialize)]
pub struct AppUpdateInstallError {
    stage: AppUpdateErrorStage,
    message: String,
}

impl AppUpdateInstallError {
    fn new(stage: AppUpdateErrorStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }

    fn check(message: impl Into<String>) -> Self {
        Self::new(AppUpdateErrorStage::Check, message)
    }
}

fn build_updater(app: &AppHandle) -> Result<Updater, String> {
    let certificate = reqwest::Certificate::from_pem(UPDATE_CA_PEM)
        .map_err(|error| format!("内置更新服务器证书无效：{error}"))?;

    app.updater_builder()
        .timeout(UPDATE_TIMEOUT)
        .configure_client(move |client| client.add_root_certificate(certificate.clone()))
        .build()
        .map_err(|error| format!("更新器初始化失败：{error}"))
}

fn update_info(update: &Update) -> AppUpdateInfo {
    AppUpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone(),
        published_at: update.date.map(|date| date.to_string()),
        mandatory: update
            .raw_json
            .get("mandatory")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

fn emit_progress(app: &AppHandle, event: &'static str, downloaded: u64, total: Option<u64>) {
    let _ = app.emit(
        UPDATE_PROGRESS_EVENT,
        AppUpdateProgress {
            event,
            downloaded,
            total,
        },
    );
}

/// Check the signed static manifest and let Tauri apply its SemVer comparison.
/// Network failures are returned to the caller; startup checks intentionally
/// handle those silently while manual checks surface them in the UI.
#[tauri::command]
pub async fn app_update_check(app: AppHandle) -> Result<AppUpdateCheck, String> {
    let current_version = app.package_info().version.to_string();
    let update = build_updater(&app)?
        .check()
        .await
        .map_err(|error| format!("无法连接更新服务器：{error}"))?;

    Ok(AppUpdateCheck {
        current_version,
        checked_at: chrono::Utc::now().to_rfc3339(),
        update: update.as_ref().map(update_info),
    })
}

/// Re-check immediately before installation so a stale UI can never install a
/// different release than the one the user accepted. The official updater then
/// downloads, verifies the minisign signature, installs, and restarts.
#[tauri::command]
pub async fn app_update_install(
    app: AppHandle,
    expected_version: String,
) -> Result<(), AppUpdateInstallError> {
    let updater = build_updater(&app).map_err(AppUpdateInstallError::check)?;
    let update = updater
        .check()
        .await
        .map_err(|error| AppUpdateInstallError::check(format!("安装前检查更新失败：{error}")))?
        .ok_or_else(|| AppUpdateInstallError::check("更新已撤回或当前版本已经是最新版"))?;

    if update.version != expected_version {
        return Err(AppUpdateInstallError::check(format!(
            "服务器版本已从 {expected_version} 变更为 {}，请重新确认更新",
            update.version
        )));
    }

    let mut downloaded = 0;

    emit_progress(&app, "started", 0, None);
    let bytes = update
        .download(
            |chunk_length, content_length| {
                downloaded += chunk_length as u64;
                emit_progress(&app, "progress", downloaded, content_length);
            },
            || {},
        )
        .await
        .map_err(|error| {
            AppUpdateInstallError::new(
                AppUpdateErrorStage::Download,
                format!("更新下载或签名校验失败：{error}"),
            )
        })?;

    // `download` verifies the signature before returning. Publish the install
    // phase only after that succeeds, and return authoritative error stages.
    emit_progress(&app, "downloaded", downloaded, None);
    crate::process_supervisor::with_external_installer_launch(|| update.install(bytes))
        .map_err(|error| {
            AppUpdateInstallError::new(
                AppUpdateErrorStage::Install,
                format!("无法启动更新安装器：{error}"),
            )
        })?
        .map_err(|error| {
            AppUpdateInstallError::new(
                AppUpdateErrorStage::Install,
                format!("更新安装失败：{error}"),
            )
        })?;

    emit_progress(&app, "installed", downloaded, None);

    // On Windows the updater exits after starting the installer and never
    // reaches this line. macOS/Linux replace the bundle in-place and need an
    // explicit restart.
    app.restart();
}
