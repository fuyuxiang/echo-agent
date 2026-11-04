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

    let builder = app
        .updater_builder()
        .timeout(UPDATE_TIMEOUT)
        .configure_client(move |client| client.add_root_certificate(certificate.clone()));
    #[cfg(windows)]
    let builder = builder.executable_path(crate::windows_install::updater_executable()?);
    builder
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

fn check_error(error: tauri_plugin_updater::Error) -> String {
    if let tauri_plugin_updater::Error::Reqwest(ref request) = error {
        if request.status() == Some(reqwest::StatusCode::GONE) {
            return "更新服务返回 HTTP 410，暂时无法检查更新。请联系管理员检查 123.56.188.16:8787 的客户端更新发布路由，然后重试。".into();
        }
    }
    format!("检查更新失败，请稍后重试：{error}")
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
    let update = build_updater(&app)?.check().await.map_err(check_error)?;

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
        .map_err(|error| AppUpdateInstallError::check(check_error(error)))?
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
    crate::prepare_application_exit(&app)
        .await
        .map_err(|error| {
            AppUpdateInstallError::new(AppUpdateErrorStage::Install, format!("更新未安装：{error}"))
        })?;
    #[cfg(not(windows))]
    if let Err(error) = crate::process_supervisor::permit_application_relaunch() {
        let message = format!("无法准备更新后重启：{error}");
        crate::abort_prepared_exit(&app, message.clone(), false);
        return Err(AppUpdateInstallError::new(
            AppUpdateErrorStage::Install,
            message,
        ));
    }
    // Flush recordings, sessions and owned IDE/automation processes before a
    // Windows installer is launched or another platform replaces our bundle.
    if let Err(message) = crate::shutdown_application_for_exit(&app).await {
        crate::abort_prepared_exit(&app, message.clone(), true);
        return Err(AppUpdateInstallError::new(
            AppUpdateErrorStage::Install,
            message,
        ));
    }
    let installed = tauri::async_runtime::spawn_blocking(move || {
        crate::process_supervisor::with_external_installer_launch(|| {
            update.install_without_exit(bytes)
        })
        .map_err(|error| format!("无法启动更新安装器：{error}"))?
        .map_err(|error| format!("更新安装失败：{error}"))
    })
    .await
    .map_err(|error| format!("更新安装任务失败：{error}"))
    .and_then(|result| result);
    if let Err(message) = installed {
        crate::abort_prepared_exit(&app, message.clone(), true);
        return Err(AppUpdateInstallError::new(
            AppUpdateErrorStage::Install,
            message,
        ));
    }

    emit_progress(
        &app,
        if cfg!(windows) {
            "installer-started"
        } else {
            "installed"
        },
        downloaded,
        None,
    );

    // Windows already handed off to the validated external installer. Other
    // platforms replaced the bundle in-place and need a controlled restart.
    crate::complete_prepared_exit(app, !cfg!(windows));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn http_410_is_preserved_by_the_updater_and_explained_to_the_user() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/manifest.json", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": "test-only-no-download",
                "dangerousInsecureTransportProtocol": true
            }),
        );
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        let updater = app
            .updater_builder()
            .timeout(Duration::from_secs(3))
            .configure_client(|client| client.no_proxy())
            .endpoints(vec![endpoint.parse().unwrap()])
            .unwrap()
            .build()
            .unwrap();
        let error = match updater.check().await {
            Err(error) => error,
            Ok(_) => panic!("An unavailable update route must not report up-to-date"),
        };
        assert!(
            matches!(&error, tauri_plugin_updater::Error::Reqwest(request) if request.status() == Some(reqwest::StatusCode::GONE))
        );
        let message = check_error(error);
        assert!(message.contains("HTTP 410"));
        assert!(message.contains("123.56.188.16:8787"));
        assert!(message.contains("发布路由"));
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
}
