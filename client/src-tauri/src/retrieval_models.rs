//! Read-only bundled models, with verified repair/import into user data only.
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    pipeline_version: String,
    assets: Vec<Asset>,
}
#[derive(Clone, Deserialize)]
struct Asset {
    repo: String,
    revision: String,
    remote: String,
    path: String,
    size: u64,
    sha256: String,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub state: String,
    pub source: String,
    pub message: Option<String>,
    /// A failed maintenance action does not invalidate a healthy active model.
    pub operation_error: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub embedding_ready: bool,
    pub reranker_ready: bool,
}
#[derive(Default)]
struct State {
    status: ModelStatus,
    running: bool,
    cancellation: CancellationToken,
    load_failure: Option<(PathBuf, String)>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
#[derive(Default)]
struct Locations {
    bundled: Option<PathBuf>,
    rejected_bundle: bool,
}
static LOCATIONS: OnceLock<Mutex<Locations>> = OnceLock::new();
fn locations() -> &'static Mutex<Locations> {
    LOCATIONS.get_or_init(Default::default)
}
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(Default::default)
}
fn manifest() -> Manifest {
    serde_json::from_str(echo_agent_memory::local::MODEL_ASSETS)
        .expect("bundled retrieval model manifest")
}

fn user_root() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("models/retrieval-v1")
}
fn pair_ready(root: &Path) -> bool {
    echo_agent_memory::local::model_files_ready(root, false)
        && echo_agent_memory::local::model_files_ready(root, true)
}
fn select_root(user: PathBuf, bundled: Option<&Path>) -> PathBuf {
    // A verified explicit repair/import overrides resources; ordinary installs
    // read the bundle directly and never copy 400MB into the user's profile.
    if pair_ready(&user) {
        return user;
    }
    bundled
        .filter(|root| pair_ready(root))
        .map(Path::to_owned)
        .unwrap_or(user)
}
pub fn initialize_from_resources(app: &AppHandle) -> PathBuf {
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|root| root.join("retrieval-models"));
    #[cfg(debug_assertions)]
    let bundled = {
        let staged = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/retrieval-models");
        if pair_ready(&staged) {
            Some(staged)
        } else {
            bundled
        }
    };
    locations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .bundled = bundled.map(|path| path.canonicalize().unwrap_or(path));
    initialize()
}
pub fn initialize() -> PathBuf {
    let guard = locations().lock().unwrap_or_else(|e| e.into_inner());
    let bundled = if guard.rejected_bundle {
        None
    } else {
        guard.bundled.as_deref()
    };
    let root = select_root(user_root(), bundled);
    echo_agent_memory::local::configure(root.clone());
    root
}
fn update(edit: impl FnOnce(&mut ModelStatus)) {
    edit(&mut state().lock().unwrap_or_else(|e| e.into_inner()).status);
}
fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}
fn prepare_directory(root: &Path) -> Result<(), String> {
    for directory in [
        root.to_owned(),
        root.join("embedding"),
        root.join("reranker"),
    ] {
        if std::fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("模型目录不能是符号链接".into());
        }
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        crate::paths::harden_private_dir(&directory)?;
    }
    Ok(())
}
fn verify(path: &Path, asset: &Asset) -> Result<(), String> {
    if !regular(path) {
        return Err(format!("模型文件缺失：{}", asset.path));
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != asset.size {
        return Err(format!("模型文件大小不完整：{}", asset.path));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if hex::encode(hash.finalize()) != asset.sha256 {
        return Err(format!("模型文件校验未通过：{}", asset.path));
    }
    Ok(())
}

#[tauri::command]
pub fn retrieval_models_status() -> ModelStatus {
    let root = initialize();
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    status_snapshot(
        guard.status.clone(),
        guard.running,
        &root,
        &user_root(),
        guard
            .load_failure
            .as_ref()
            .filter(|(failed, _)| failed == &root)
            .map(|(_, error)| error.as_str()),
        echo_agent_memory::local::device_supported(),
    )
}

fn status_snapshot(
    mut status: ModelStatus,
    running: bool,
    root: &Path,
    user: &Path,
    load_failure: Option<&str>,
    supported: bool,
) -> ModelStatus {
    status.total_bytes = manifest().assets.iter().map(|asset| asset.size).sum();
    status.source = if pair_ready(root) {
        if root == user {
            "repaired"
        } else {
            "bundled"
        }
    } else {
        "missing"
    }
    .into();
    status.embedding_ready = echo_agent_memory::local::model_files_ready(root, false);
    status.reranker_ready = echo_agent_memory::local::model_files_ready(root, true);
    if !supported {
        status.state = "unsupported".into();
        status.message =
            Some("当前电脑暂不支持本地语义模型，已使用关键词检索；知识库和记忆仍可使用".into());
        return status;
    }
    if !running && load_failure.is_some() {
        status.state = "error".into();
        status.message =
            load_failure.map(|error| format!("本地模型加载失败，已使用关键词检索：{error}"));
    } else if !running && status.embedding_ready && status.reranker_ready {
        status.state = "ready".into();
        status.downloaded_bytes = status.total_bytes;
    } else if !running && status.operation_error.is_some() {
        status.state = "error".into();
    } else if !running || status.state.is_empty() {
        status.state = "missing".into();
    }
    status
}

#[tauri::command]
pub fn retrieval_models_cancel() -> bool {
    let state = state().lock().unwrap_or_else(|e| e.into_inner());
    if state.running {
        state.cancellation.cancel();
        true
    } else {
        false
    }
}

fn begin() -> Result<CancellationToken, String> {
    if !echo_agent_memory::local::device_supported() {
        return Err("本机暂不支持本地语义模型，请继续使用关键词检索".into());
    }
    let mut state = state().lock().map_err(|_| "模型准备状态不可用")?;
    if state.running {
        return Err("本地模型正在准备，请稍候".into());
    }
    state.running = true;
    state.cancellation = CancellationToken::new();
    state.status = ModelStatus {
        state: "downloading".into(),
        total_bytes: manifest().assets.iter().map(|a| a.size).sum(),
        ..Default::default()
    };
    Ok(state.cancellation.clone())
}

#[tauri::command]
pub fn retrieval_models_prepare(app: AppHandle) -> Result<ModelStatus, String> {
    let cancellation = begin()?;
    tauri::async_runtime::spawn(async move {
        let result = check_or_repair(&cancellation)
            .await
            .map_err(|error| format!("模型检查与修复未完成：{error}"));
        finish(result, &cancellation, app);
    });
    Ok(retrieval_models_status())
}

#[tauri::command]
pub fn retrieval_models_import(app: AppHandle, directory: String) -> Result<ModelStatus, String> {
    let directory = std::fs::canonicalize(directory).map_err(|_| "模型文件夹不存在")?;
    if !directory.is_dir() {
        return Err("请选择包含 embedding 和 reranker 的模型文件夹".into());
    }
    let cancellation = begin()?;
    tauri::async_runtime::spawn(async move {
        let result = install(Some(directory), &cancellation)
            .await
            .map_err(|error| format!("离线模型导入未完成：{error}"));
        finish(result, &cancellation, app);
    });
    Ok(retrieval_models_status())
}

fn finish(result: Result<(), String>, cancellation: &CancellationToken, app: AppHandle) {
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    state.running = false;
    match result {
        Ok(()) => {
            state.status.state = "ready".into();
            state.status.message = None;
            state.status.operation_error = None;
            state.status.embedding_ready = true;
            state.status.reranker_ready = true;
        }
        Err(_) if cancellation.is_cancelled() => {
            state.status.state = "missing".into();
            state.status.message = Some("已暂停模型维护，可稍后继续。".into());
        }
        Err(error) => {
            state.status.state = "error".into();
            state.status.message = None;
            state.status.operation_error = Some(error);
        }
    }
    let ready = state.status.state == "ready";
    drop(state);
    if ready {
        // The model fingerprint selects the right index generation. A healthy
        // maintenance check must not re-extract every file or discard vectors.
        crate::personal_knowledge::schedule_rebuild(app, false);
    }
}

async fn download_asset(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    size: u64,
    completed: u64,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let mut offset = std::fs::metadata(part).map_or(0, |m| m.len());
    if offset > size {
        tokio::fs::remove_file(&part)
            .await
            .map_err(|e| e.to_string())?;
        offset = 0;
    }
    if offset < size {
        let mut request = client.get(url);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response = tokio::select! { _=cancellation.cancelled()=>return Err("cancelled".into()), result=request.send()=>result.map_err(|_|"模型下载连接失败，可重试或导入离线模型".to_owned())? };
        if !response.status().is_success() {
            return Err(format!(
                "模型下载失败（HTTP {}），可重试或离线导入",
                response.status().as_u16()
            ));
        }
        if offset > 0 {
            if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                let range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if !range.starts_with(&format!("bytes {offset}-")) {
                    return Err("模型下载续传范围无效，请重试".into());
                }
            } else {
                offset = 0;
            }
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(offset > 0)
            .truncate(offset == 0)
            .open(part)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = response.bytes_stream();
        loop {
            let block = tokio::select! {_=cancellation.cancelled()=>return Err("cancelled".into()), block=stream.next()=>block};
            let Some(block) = block else {
                break;
            };
            let block = block.map_err(|_| "模型下载中断，可点击重试继续".to_owned())?;
            offset += block.len() as u64;
            if offset > size {
                return Err("模型下载内容超过预期大小".into());
            }
            file.write_all(&block).await.map_err(|e| e.to_string())?;
            update(|s| s.downloaded_bytes = completed + offset);
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn warmup_at(root: &Path, cancellation: &CancellationToken) -> Result<(), String> {
    let result = tokio::select! {
        _=cancellation.cancelled()=>Err("cancelled".to_owned()),
        result=echo_agent_memory::local::warmup()=>result,
    };
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = &result {
        if !cancellation.is_cancelled() {
            state.load_failure = Some((root.to_owned(), error.clone()));
        }
    } else if state
        .load_failure
        .as_ref()
        .is_some_and(|(failed, _)| failed == root)
    {
        state.load_failure = None;
    }
    result
}

async fn check_or_repair(cancellation: &CancellationToken) -> Result<(), String> {
    for _ in 0..2 {
        let root = initialize();
        if !pair_ready(&root) {
            break;
        }
        update(|s| {
            s.state = "verifying".into();
            s.message = Some("正在检查本地模型".into());
        });
        let check_root = root.clone();
        let valid = tokio::task::spawn_blocking(move || {
            for asset in manifest().assets {
                verify(&check_root.join(&asset.path), &asset)?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())?
        .is_ok();
        if cancellation.is_cancelled() {
            return Err("cancelled".into());
        }
        if valid {
            return warmup_at(&root, cancellation).await;
        }
        if root == user_root() {
            tokio::fs::remove_file(root.join("verified.json"))
                .await
                .map_err(|e| e.to_string())?;
            // A broken user override can be discarded without downloading if
            // the installer still contains a good model pair.
        } else {
            locations()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .rejected_bundle = true;
        }
    }
    install(None, cancellation).await
}

async fn install(source: Option<PathBuf>, cancellation: &CancellationToken) -> Result<(), String> {
    let source = source
        .map(std::fs::canonicalize)
        .transpose()
        .map_err(|e| e.to_string())?;
    if let Some(source) = source.clone() {
        update(|s| {
            s.state = "verifying".into();
            s.message = Some("正在校验离线模型".into());
        });
        let token = cancellation.clone();
        tokio::task::spawn_blocking(move || validate_offline_source(&source, &token))
            .await
            .map_err(|e| e.to_string())??;
    }
    // Never write to app resources: modifying them would invalidate signing.
    let root = user_root();
    prepare_directory(&root)?;
    let manifest = manifest();
    let mut completed = 0u64;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| e.to_string())?;
    for asset in &manifest.assets {
        if cancellation.is_cancelled() {
            return Err("cancelled".into());
        }
        let target = root.join(&asset.path);
        let check_target = target.clone();
        let check_asset = asset.clone();
        if tokio::task::spawn_blocking(move || verify(&check_target, &check_asset))
            .await
            .map_err(|e| e.to_string())?
            .is_ok()
        {
            completed += asset.size;
            update(|s| s.downloaded_bytes = completed);
            continue;
        }
        // A corrupt asset invalidates readiness before any repair connection.
        // A failed/offline repair must never leave the old verified marker usable.
        match tokio::fs::remove_file(root.join("verified.json")).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let part = target.with_extension("part");
        if std::fs::symlink_metadata(&part).is_ok_and(|m| m.file_type().is_symlink())
            || std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("模型文件不能是符号链接".into());
        }
        let bundled = locations()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bundled
            .clone();
        let source = if source.is_some() {
            source.clone()
        } else if let Some(bundled) = bundled {
            let check_root = bundled.clone();
            let check_asset = asset.clone();
            if tokio::task::spawn_blocking(move || {
                verify(&check_root.join(&check_asset.path), &check_asset)
            })
            .await
            .map_err(|e| e.to_string())?
            .is_ok()
            {
                Some(bundled)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(source) = &source {
            update(|s| {
                s.state = "verifying".into();
                s.message = Some("正在校验离线模型".into());
            });
            let from = source.join(&asset.path);
            if !regular(&from)
                || !from
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .starts_with(source)
            {
                return Err(format!("离线模型文件无效：{}", asset.path));
            }
            let copied = asset.clone();
            let destination = part.clone();
            tokio::task::spawn_blocking(move || {
                verify(&from, &copied)?;
                std::fs::copy(from, destination).map_err(|e| e.to_string())?;
                Ok::<_, String>(())
            })
            .await
            .map_err(|e| e.to_string())??;
        } else {
            update(|s| {
                s.state = "downloading".into();
                s.message = None;
            });
            let url = format!(
                "https://huggingface.co/{}/resolve/{}/{}?download=true",
                asset.repo, asset.revision, asset.remote
            );
            download_asset(&client, &url, &part, asset.size, completed, cancellation).await?;
        }
        update(|s| {
            s.state = "verifying".into();
            s.message = Some("正在校验本地模型".into());
        });
        let verify_path = part.clone();
        let verify_asset = asset.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || verify(&verify_path, &verify_asset))
            .await
            .map_err(|e| e.to_string())?
        {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(error);
        }
        if cancellation.is_cancelled() {
            return Err("cancelled".into());
        }
        tokio::fs::rename(&part, &target)
            .await
            .map_err(|e| e.to_string())?;
        completed += asset.size;
        update(|s| {
            s.state = "verifying".into();
            s.downloaded_bytes = completed;
        });
    }
    if manifest.pipeline_version != echo_agent_memory::local::PIPELINE_VERSION {
        return Err("模型配置版本不匹配".into());
    }
    crate::paths::write_private_file(
        &root.join("verified.json"),
        echo_agent_memory::local::MODEL_ASSETS.as_bytes(),
    )?;
    initialize();
    update(|s| {
        s.state = "verifying".into();
        s.message = Some("正在检查本机模型运行能力".into());
    });
    let result = warmup_at(&root, cancellation).await;
    // Cancellation stops inference, not checksum validity. Keep a verified
    // package available when the user pauses maintenance during warmup.
    if result.is_err() && !cancellation.is_cancelled() {
        let _ = tokio::fs::remove_file(root.join("verified.json")).await;
        initialize();
    }
    result
}

fn validate_offline_source(source: &Path, cancellation: &CancellationToken) -> Result<(), String> {
    for asset in manifest().assets {
        if cancellation.is_cancelled() {
            return Err("cancelled".into());
        }
        let path = source.join(&asset.path);
        if !regular(&path)
            || !path
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(source)
        {
            return Err(format!("离线模型文件无效：{}", asset.path));
        }
        verify(&path, &asset)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sparse_package(root: &Path) {
        for asset in manifest().assets {
            let path = root.join(asset.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::File::create(path)
                .unwrap()
                .set_len(asset.size)
                .unwrap();
        }
        std::fs::write(
            root.join("verified.json"),
            echo_agent_memory::local::MODEL_ASSETS,
        )
        .unwrap();
    }

    #[test]
    fn maintenance_failure_preserves_healthy_active_models_but_load_failures_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let bundled = directory.path().join("bundled");
        let user = directory.path().join("user");
        sparse_package(&bundled);
        let failed_action = || ModelStatus {
            state: "error".into(),
            operation_error: Some("离线模型导入未完成：文件缺失".into()),
            ..Default::default()
        };
        let status = status_snapshot(failed_action(), false, &bundled, &user, None, true);
        assert_eq!(status.state, "ready");
        assert_eq!(status.source, "bundled");
        assert!(status.embedding_ready && status.reranker_ready);
        assert!(status.operation_error.unwrap().contains("导入未完成"));
        assert!(!user.exists());

        let status = status_snapshot(
            failed_action(),
            false,
            &bundled,
            &user,
            Some("CPU inference failed"),
            true,
        );
        assert_eq!(status.state, "error");
        assert!(status.message.unwrap().contains("CPU inference failed"));
        let missing = status_snapshot(failed_action(), false, &user, &user, None, true);
        assert_eq!(missing.state, "error");
        assert!(!missing.embedding_ready && !missing.reranker_ready);
        let removed = status_snapshot(
            ModelStatus {
                state: "ready".into(),
                ..Default::default()
            },
            false,
            &user,
            &user,
            None,
            true,
        );
        assert_eq!(removed.state, "missing");
    }

    #[test]
    fn cancelled_maintenance_keeps_ready_models_and_inflight_progress_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        sparse_package(directory.path());
        let status = status_snapshot(
            ModelStatus {
                state: "missing".into(),
                message: Some("已暂停模型维护".into()),
                ..Default::default()
            },
            false,
            directory.path(),
            Path::new("/missing-user-models"),
            None,
            true,
        );
        assert_eq!(status.state, "ready");
        assert_eq!(status.message.as_deref(), Some("已暂停模型维护"));
        let running = status_snapshot(
            ModelStatus {
                state: "verifying".into(),
                ..Default::default()
            },
            true,
            directory.path(),
            directory.path(),
            None,
            true,
        );
        assert_eq!(running.state, "verifying");
        let unsupported = status_snapshot(
            ModelStatus::default(),
            false,
            directory.path(),
            directory.path(),
            None,
            false,
        );
        assert_eq!(unsupported.state, "unsupported");
    }

    #[test]
    fn fresh_install_uses_read_only_bundle_and_only_verified_complete_repairs_override_it() {
        let directory = tempfile::tempdir().unwrap();
        let bundled = directory.path().join("app/Resources/retrieval-models");
        let user = directory.path().join("user/models/retrieval-v1");
        sparse_package(&bundled);
        assert_eq!(select_root(user.clone(), Some(&bundled)), bundled);
        assert!(
            !user.exists(),
            "startup must not copy weights or create user directories"
        );
        sparse_package(&user);
        std::fs::remove_file(user.join("verified.json")).unwrap();
        assert_eq!(select_root(user.clone(), Some(&bundled)), bundled);
        std::fs::write(
            user.join("verified.json"),
            echo_agent_memory::local::MODEL_ASSETS,
        )
        .unwrap();
        assert_eq!(select_root(user.clone(), Some(&bundled)), user);
        std::fs::remove_file(user.join("reranker/model.onnx")).unwrap();
        assert_eq!(select_root(user.clone(), Some(&bundled)), bundled);
        assert_eq!(select_root(user.clone(), None), user);
    }

    #[tokio::test]
    #[ignore = "requires real bundled models and an isolated echo-retrieval-bundle-* data directory"]
    async fn bundled_models_work_offline_without_creating_or_copying_user_models() {
        use echo_agent_memory::embedding::EmbeddingProvider;
        let home = crate::paths::echo_agent_home_dir();
        assert!(home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("echo-retrieval-bundle-")));
        assert!(!user_root().exists());
        let bundle =
            PathBuf::from(std::env::var_os("ECHO_RETRIEVAL_MODELS").expect("bundled models"))
                .canonicalize()
                .unwrap();
        let marker_time = std::fs::metadata(bundle.join("verified.json"))
            .unwrap()
            .modified()
            .unwrap();
        locations().lock().unwrap().bundled = Some(bundle.clone());
        assert_eq!(initialize(), bundle);
        // Repeated runtime/personal-knowledge initialization must keep the bundle.
        assert_eq!(initialize(), bundle);
        let status = retrieval_models_status();
        assert_eq!(status.state, "ready");
        assert_eq!(status.source, "bundled");
        let provider = echo_agent_memory::local::LocalEmbeddingProvider::new().unwrap();
        let long = "员工应按公司制度提交申请并保留相关凭据。".repeat(60);
        assert!(
            echo_agent_memory::local::token_count(&long) > 700,
            "token counting must not truncate at the inference limit"
        );
        let chunks = echo_agent_memory::chunker::chunk_markdown(&long, &Default::default());
        assert!(chunks.len() > 1);
        assert!(chunks
            .iter()
            .all(|chunk| echo_agent_memory::local::token_count(&chunk.text) <= 384));
        assert_eq!(
            provider
                .embed_query("离职时应归还哪些物品？")
                .await
                .unwrap()
                .len(),
            768
        );
        let scores = echo_agent_memory::local::rerank(
            "离职归还物品",
            &["离职员工须交还电脑和门禁卡", "北京酒店标准为每晚500元"],
            None,
        )
        .await
        .unwrap();
        assert!(scores[0] > scores[1]);
        let documents = [
            "员工出差前须在办公系统提交差旅申请，经直属主管审批后，财务方可报销交通和住宿费用。",
            "生产数据库连接失败时，先检查网络和防火墙，再核对数据库地址、端口和账号权限。",
            "年假申请应提前三个工作日提交，由部门负责人审批。",
            "离职人员须交还电脑、门禁卡，办理账号停用和工作交接。",
        ];
        let vectors = provider.embed_batch(&documents).await.unwrap();
        for (query, expected) in [
            ("离开公司时要归还哪些物品？", 3),
            ("数据库连不上应该排查哪些地方？", 1),
        ] {
            let query_vector = provider.embed_query(query).await.unwrap();
            let mut order = (0..documents.len()).collect::<Vec<_>>();
            let similarity = |i: usize| {
                query_vector
                    .iter()
                    .zip(&vectors[i])
                    .map(|(a, b)| (*a as f64) * (*b as f64))
                    .sum::<f64>()
            };
            order.sort_by(|a, b| similarity(*b).total_cmp(&similarity(*a)));
            assert!(
                order[..3].contains(&expected),
                "semantic recall missed {query}"
            );
            let relevance = echo_agent_memory::local::rerank(query, &documents, None)
                .await
                .unwrap();
            let best = (0..relevance.len())
                .max_by(|a, b| relevance[*a].total_cmp(&relevance[*b]))
                .unwrap();
            assert_eq!(best, expected, "reranking missed {query}");
            assert!(relevance[expected] >= echo_agent_memory::local::MIN_RELEVANCE);
        }
        let irrelevant =
            echo_agent_memory::local::rerank("火星卫星的轨道周期是多少？", &documents, None)
                .await
                .unwrap();
        assert!(irrelevant
            .iter()
            .all(|score| *score < echo_agent_memory::local::MIN_RELEVANCE));
        // Maintenance of a healthy bundle also stays fully offline and read-only.
        check_or_repair(&CancellationToken::new()).await.unwrap();
        assert!(!user_root().exists());
        // An accidental bad import must not create/modify user models or
        // replace a working bundled pipeline with an unusable override.
        let invalid = tempfile::tempdir().unwrap();
        assert!(
            install(Some(invalid.path().to_owned()), &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(!user_root().exists());
        assert_eq!(initialize(), bundle);
        assert_eq!(
            provider
                .embed_query("导入失败后仍能检索")
                .await
                .unwrap()
                .len(),
            768
        );
        assert_eq!(
            std::fs::metadata(bundle.join("verified.json"))
                .unwrap()
                .modified()
                .unwrap(),
            marker_time
        );
    }

    #[tokio::test]
    #[ignore = "requires a verified offline package and an isolated echo-retrieval-install-* data directory"]
    async fn offline_package_installs_verified_models_and_runs_inference() {
        use echo_agent_memory::embedding::EmbeddingProvider;
        let home = crate::paths::echo_agent_home_dir();
        assert!(
            home.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("echo-retrieval-install-")),
            "refuse a non-isolated model install"
        );
        let source = PathBuf::from(
            std::env::var_os("ECHO_RETRIEVAL_MODELS").expect("verified offline package"),
        );
        let cancellation = CancellationToken::new();
        install(Some(source), &cancellation).await.unwrap();
        let status = retrieval_models_status();
        assert_eq!(status.state, "ready");
        assert!(status.embedding_ready && status.reranker_ready);
        for asset in manifest().assets {
            verify(&initialize().join(&asset.path), &asset).unwrap();
        }
        let provider = echo_agent_memory::local::LocalEmbeddingProvider::new().unwrap();
        let vector = provider
            .embed_query("离职时应归还哪些物品？")
            .await
            .unwrap();
        assert_eq!(vector.len(), 768);
        let scores = echo_agent_memory::local::rerank(
            "离职归还物品",
            &["离职员工须交还电脑和门禁卡", "北京酒店标准为每晚500元"],
            None,
        )
        .await
        .unwrap();
        assert!(scores[0] > scores[1]);
    }

    #[test]
    fn assets_are_fixed_and_never_resolve_floating_models() {
        let manifest = manifest();
        assert_eq!(manifest.assets.len(), 4);
        for asset in manifest.assets {
            assert_eq!(asset.revision.len(), 40);
            assert_eq!(asset.sha256.len(), 64);
            assert!(!asset.path.contains(".."));
        }
    }
    async fn response_server(response: &str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let response = response.to_owned();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            while !bytes.windows(4).any(|value| value == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (url, task)
    }
    #[tokio::test]
    async fn download_resumes_and_handles_servers_ignoring_range() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for response in [
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef",
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let part = directory.path().join("model.part");
            std::fs::write(&part, b"abc").unwrap();
            let (url, request) = response_server(response).await;
            download_asset(&client, &url, &part, 6, 0, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(std::fs::read(&part).unwrap(), b"abcdef");
            assert!(
                request
                    .await
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("range: bytes=3-")
            );
        }
    }
    #[tokio::test]
    async fn invalid_resume_and_oversized_downloads_cannot_replace_verified_assets() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("model.part");
        std::fs::write(&part, b"abc").unwrap();
        let (url,request)=response_server("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef").await;
        assert!(
            download_asset(&client, &url, &part, 6, 0, &CancellationToken::new())
                .await
                .is_err()
        );
        request.await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"abc");
        std::fs::remove_file(&part).unwrap();
        let (url, request) = response_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nabcdefg",
        )
        .await;
        assert!(
            download_asset(&client, &url, &part, 6, 0, &CancellationToken::new())
                .await
                .is_err()
        );
        request.await.unwrap();
        assert!(std::fs::metadata(&part).unwrap().len() <= 6);
    }
    #[test]
    fn offline_assets_require_exact_size_and_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.onnx");
        std::fs::write(&path, b"abcdef").unwrap();
        let asset = Asset {
            repo: String::new(),
            revision: String::new(),
            remote: String::new(),
            path: "model.onnx".into(),
            size: 6,
            sha256: hex::encode(Sha256::digest(b"abcdef")),
        };
        assert!(verify(&path, &asset).is_ok());
        std::fs::write(&path, b"abcdeg").unwrap();
        assert!(verify(&path, &asset).is_err());
        std::fs::write(&path, b"abc").unwrap();
        assert!(verify(&path, &asset).is_err());
    }
}
