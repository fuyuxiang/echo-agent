//! Persisted WebDAV storage providers and backend I/O.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use fs2::FileExt;
use futures::StreamExt;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:displayname/><D:resourcetype/>
<D:getcontentlength/><D:getlastmodified/><D:getcontenttype/></D:prop></D:propfind>"#;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_PROPFIND_BODY_BYTES: usize = 5 * 1024 * 1024;
const MAX_TEXT_BODY_BYTES: usize = 5 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_STORAGE_CONFIG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STORAGE_PROVIDERS: usize = 64;
const MAX_REMOTE_PATH_CHARS: usize = 4_096;
const MAX_REMOTE_PATH_SEGMENTS: usize = 256;
const MAX_PROPFIND_ENTRIES: usize = 5_000;
const MAX_LOCAL_PATH_CHARS: usize = 32_768;
const STORAGE_LOCK_ATTEMPTS: usize = 200;
const STORAGE_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const STORED_PASSWORD_MASK: &str = "••••";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageProviderConfig {
    pub id: String,
    pub label: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub enabled: bool,
}

fn default_kind() -> String {
    "webdav".into()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StorageConfigStore {
    #[serde(default)]
    providers: Vec<StorageProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub modified_at: Option<i64>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Default)]
struct DavResponse {
    href: String,
    display_name: String,
    is_dir: bool,
    size: Option<u64>,
    modified_at: Option<i64>,
    mime_type: Option<String>,
}

static ACCESS: OnceLock<Mutex<()>> = OnceLock::new();
fn access() -> &'static Mutex<()> {
    ACCESS.get_or_init(|| Mutex::new(()))
}

fn config_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-storage-providers.json")
}

fn store_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn acquire_store_file_lock_at(path: &Path) -> Result<std::fs::File, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("存储源配置目录必须是真实目录，不能是符号链接".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建存储源配置目录失败：{error}"))?;
            let metadata = std::fs::symlink_metadata(parent)
                .map_err(|error| format!("检查存储源配置目录失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("存储源配置目录必须是真实目录，不能是符号链接".into());
            }
        }
        Err(error) => return Err(format!("检查存储源配置目录失败：{error}")),
    }

    let lock_path = store_lock_path(path);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("存储源配置锁必须是普通文件，不能是符号链接".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("检查存储源配置锁失败：{error}")),
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(&lock_path)
        .map_err(|error| format!("打开存储源配置锁失败：{error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("读取存储源配置锁失败：{error}"))?
        .is_file()
    {
        return Err("存储源配置锁在打开期间被替换".into());
    }

    for _ in 0..STORAGE_LOCK_ATTEMPTS {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(STORAGE_LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(format!("锁定存储源配置失败：{error}")),
        }
    }
    Err("等待其他 EchoAgent 实例保存存储源配置超时，请稍后重试".into())
}

/// Serialize every operation that may quarantine or replace the provider
/// store. The fixed process-lock -> file-lock order prevents nested deadlocks,
/// while the separate stable lock pathname survives atomic store replacement.
fn with_store_access_at<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _process_guard = access()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _file_guard = acquire_store_file_lock_at(path)?;
    operation()
}

fn quarantine_invalid_store(path: &std::path::Path) {
    let backup = path.with_extension(format!("json.corrupt.{}", uuid::Uuid::now_v7().simple()));
    if let Err(error) = std::fs::rename(path, &backup) {
        tracing::warn!(path = %path.display(), %error, "failed to quarantine invalid storage provider config");
    } else {
        let _ = crate::paths::harden_private_file(&backup);
        tracing::warn!(path = %path.display(), backup = %backup.display(), "invalid storage provider config was quarantined");
    }
}

fn read_store_at(path: &Path) -> Result<StorageConfigStore, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StorageConfigStore::default());
        }
        Err(error) => return Err(format!("读取存储源配置失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("存储源配置必须是普通文件，不能是符号链接".into());
    }
    if metadata.len() > MAX_STORAGE_CONFIG_BYTES {
        quarantine_invalid_store(path);
        return Err("存储源配置超过 2MB 限制，已隔离保存".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(path)
        .map_err(|error| format!("打开存储源配置失败：{error}"))?;
    let mut raw = Vec::new();
    if file
        .take(MAX_STORAGE_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut raw)
        .is_err()
        || raw.len() as u64 > MAX_STORAGE_CONFIG_BYTES
    {
        tracing::warn!("storage provider config is unreadable or exceeds 2MB");
        quarantine_invalid_store(path);
        return Err("存储源配置无法读取或超过 2MB，已隔离保存".into());
    }
    match serde_json::from_slice(&raw) {
        Ok(store) => {
            let store: StorageConfigStore = store;
            if store.providers.len() > MAX_STORAGE_PROVIDERS
                || store
                    .providers
                    .iter()
                    .any(|provider| validate_shape(provider).is_err())
            {
                quarantine_invalid_store(path);
                return Err("存储源配置包含超限或不安全条目，已隔离保存".into());
            }
            Ok(store)
        }
        Err(error) => {
            tracing::warn!(%error, "storage provider config is invalid JSON");
            quarantine_invalid_store(path);
            Err("存储源配置 JSON 损坏，已隔离保存".into())
        }
    }
}

fn read_store() -> Result<StorageConfigStore, String> {
    let path = config_path();
    with_store_access_at(&path, || read_store_at(&path))
}

fn write_store_at(path: &Path, store: &StorageConfigStore) -> Result<(), String> {
    if store.providers.len() > MAX_STORAGE_PROVIDERS
        || store
            .providers
            .iter()
            .any(|provider| validate_shape(provider).is_err())
    {
        return Err("存储源配置包含超限或不安全条目，未写入磁盘".into());
    }
    let raw = serde_json::to_vec_pretty(store)
        .map_err(|e| format!("serialize storage providers: {e}"))?;
    if raw.len() as u64 > MAX_STORAGE_CONFIG_BYTES {
        return Err("存储源配置超过 2MB 限制，未写入磁盘".into());
    }
    crate::paths::write_private_file(path, &raw)
}

fn update_store_at<T>(
    path: &Path,
    update: impl FnOnce(&mut StorageConfigStore) -> Result<T, String>,
) -> Result<T, String> {
    with_store_access_at(path, || {
        let mut store = read_store_at(path)?;
        let result = update(&mut store)?;
        write_store_at(path, &store)?;
        Ok(result)
    })
}

fn validate_shape(config: &StorageProviderConfig) -> Result<(), String> {
    if config.id.trim().is_empty()
        || config.id.len() > 128
        || config.id.chars().any(char::is_control)
        || config.label.trim().is_empty()
        || config.label.len() > 512
        || config.label.chars().any(char::is_control)
        || config
            .username
            .as_ref()
            .is_some_and(|value| value.len() > 4096 || value.contains('\0'))
        || config
            .password
            .as_ref()
            .is_some_and(|value| value.len() > 64 * 1024 || value.contains('\0'))
    {
        return Err("存储源 id 和显示名不能为空".into());
    }
    if config.kind != "webdav" {
        return Err("当前版本仅支持 WebDAV".into());
    }
    if config.base_url.chars().count() > 8_192 || config.base_url.chars().any(char::is_control) {
        return Err("WebDAV URL 过长或包含控制字符".into());
    }
    Ok(())
}

fn validate(config: &StorageProviderConfig) -> Result<(), String> {
    validate_shape(config)?;
    let url = url::Url::parse(&config.base_url).map_err(|e| format!("WebDAV URL 无效：{e}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("WebDAV URL 缺少主机或包含内嵌凭据、query/fragment".into());
    }
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("WebDAV URL 必须使用 http 或 https".into());
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
            || host.trim_matches(['[', ']']) == "::1"
    });
    if url.scheme() != "https" && !loopback {
        return Err("WebDAV 凭据只能通过 HTTPS 传输（本机 localhost/回环地址除外）".into());
    }
    Ok(())
}

fn provider(id: &str) -> Result<StorageProviderConfig, String> {
    let config = read_store()?
        .providers
        .into_iter()
        .find(|p| p.id == id && p.enabled)
        .ok_or_else(|| format!("存储源 {id} 不存在或未启用"))?;
    // Revalidate persisted entries at the point of use. Older releases allowed
    // remote plaintext HTTP, so validating only new writes would still send a
    // saved Basic credential over an insecure legacy endpoint.
    validate(&config).map_err(|error| format!("存储源 {id} 配置不安全：{error}"))?;
    Ok(config)
}

fn target_url(config: &StorageProviderConfig, path: &str) -> Result<url::Url, String> {
    let path = normalize_path(path)?;
    let mut url = url::Url::parse(config.base_url.trim_end_matches('/'))
        .map_err(|e| format!("WebDAV URL 无效：{e}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "WebDAV URL 不能作为目录 URL")?;
        segments.pop_if_empty();
        for segment in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        // A configured WebDAV origin is credential-bearing. Do not replay
        // Basic auth or write methods through redirects to a different target.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

fn request(
    config: &StorageProviderConfig,
    method: reqwest::Method,
    path: &str,
) -> Result<reqwest::RequestBuilder, String> {
    let mut request = client()?.request(method, target_url(config, path)?);
    if let Some(username) = config.username.as_deref().filter(|v| !v.is_empty()) {
        request = request.basic_auth(username, config.password.clone());
    }
    Ok(request)
}

fn confirm_file_transfer(app: &AppHandle, title: &str, message: String) -> Result<(), String> {
    let approved = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "允许本次".into(),
            "取消".into(),
        ))
        .blocking_show();
    if approved {
        Ok(())
    } else {
        Err("用户取消了文件传输".into())
    }
}

async fn response_prefix(
    response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取响应失败：{error}"))?;
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        if remaining == 0 {
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() > limit {
            bytes.truncate(limit);
            return Ok((bytes, true));
        }
    }
    Ok((bytes, false))
}

async fn response_bytes_limited(
    response: reqwest::Response,
    limit: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("{label}超过 {}MB 限制", limit / 1024 / 1024));
    }
    let (bytes, truncated) = response_prefix(response, limit).await?;
    if truncated {
        Err(format!("{label}超过 {}MB 限制", limit / 1024 / 1024))
    } else {
        Ok(bytes)
    }
}

async fn response_error(response: reqwest::Response, operation: &str) -> String {
    let status = response.status();
    let (detail, truncated) = response_prefix(response, MAX_ERROR_BODY_BYTES)
        .await
        .unwrap_or_default();
    let detail = String::from_utf8_lossy(&detail);
    let suffix = if truncated { "…" } else { "" };
    format!(
        "{operation} 失败：HTTP {status} {}{suffix}",
        detail.chars().take(240).collect::<String>(),
    )
}

fn decode_href_path(href: &str) -> String {
    let raw_path = url::Url::parse(href)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| href.split('?').next().unwrap_or(href).to_string());
    urlencoding::decode(&raw_path)
        .map(|v| v.into_owned())
        .unwrap_or(raw_path)
}

fn parse_propfind(
    xml: &str,
    config: &StorageProviderConfig,
    requested_path: &str,
) -> Result<Vec<StorageEntry>, String> {
    let base_path = url::Url::parse(&config.base_url)
        .ok()
        .map(|u| decode_href_path(u.path()))
        .unwrap_or_default();
    let base_path = normalize_path(&base_path)?;
    let base_prefix = format!("{}/", base_path.trim_end_matches('/'));
    let requested = normalize_path(requested_path)?;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut current: Option<DavResponse> = None;
    let mut field = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name =
                    String::from_utf8_lossy(event.local_name().as_ref()).to_ascii_lowercase();
                if name == "response" {
                    current = Some(DavResponse::default());
                }
                field = name;
            }
            Ok(Event::Empty(event)) => {
                let name =
                    String::from_utf8_lossy(event.local_name().as_ref()).to_ascii_lowercase();
                if name == "collection" {
                    if let Some(item) = &mut current {
                        item.is_dir = true;
                    }
                }
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map(|v| v.into_owned()).unwrap_or_default();
                if let Some(item) = &mut current {
                    match field.as_str() {
                        "href" => item.href = value,
                        "displayname" => item.display_name = value,
                        "getcontentlength" => item.size = value.parse().ok(),
                        "getlastmodified" => {
                            item.modified_at = chrono::DateTime::parse_from_rfc2822(&value)
                                .ok()
                                .map(|v| v.timestamp_millis())
                        }
                        "getcontenttype" => item.mime_type = (!value.is_empty()).then_some(value),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name =
                    String::from_utf8_lossy(event.local_name().as_ref()).to_ascii_lowercase();
                if name == "response" {
                    if let Some(item) = current.take() {
                        let Ok(href_path) = normalize_path(&decode_href_path(&item.href)) else {
                            field.clear();
                            continue;
                        };
                        let relative = if base_path == "/" {
                            href_path.as_str()
                        } else if href_path == base_path {
                            "/"
                        } else if let Some(relative) = href_path.strip_prefix(&base_prefix) {
                            relative
                        } else {
                            // A DAV response must not inject entries outside the configured
                            // collection. Besides confusing the UI, accepting one would turn
                            // a later click into an operation on a different server-side path.
                            field.clear();
                            continue;
                        };
                        let Ok(path) = normalize_path(relative) else {
                            field.clear();
                            continue;
                        };
                        if path != requested {
                            let fallback_name = path
                                .trim_end_matches('/')
                                .split('/')
                                .next_back()
                                .unwrap_or_default();
                            let display_name = if item.display_name.trim().is_empty() {
                                fallback_name.to_string()
                            } else {
                                item.display_name
                                    .trim()
                                    .chars()
                                    .filter(|character| !character.is_control())
                                    .collect()
                            };
                            if !display_name.is_empty() {
                                if out.len() >= MAX_PROPFIND_ENTRIES {
                                    return Err(format!(
                                        "WebDAV 目录条目超过 {MAX_PROPFIND_ENTRIES} 条限制"
                                    ));
                                }
                                out.push(StorageEntry {
                                    path,
                                    name: display_name.chars().take(512).collect(),
                                    is_dir: item.is_dir,
                                    size: item.size,
                                    modified_at: item.modified_at,
                                    mime_type: item.mime_type.map(|mime_type| {
                                        mime_type
                                            .chars()
                                            .filter(|character| !character.is_control())
                                            .take(512)
                                            .collect()
                                    }),
                                });
                            }
                        }
                    }
                }
                field.clear();
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("WebDAV PROPFIND XML 无效：{error}")),
            _ => {}
        }
    }
    Ok(out)
}

fn normalize_path(path: &str) -> Result<String, String> {
    if path.chars().count() > MAX_REMOTE_PATH_CHARS
        || path
            .chars()
            .any(|character| character == '\0' || character.is_control())
    {
        return Err("远程路径过长或包含控制字符".into());
    }
    let normalized = path.replace('\\', "/");
    let raw_parts = normalized.split('/').filter(|part| !part.is_empty());
    let mut parts = Vec::new();
    for part in raw_parts {
        if part == "." {
            continue;
        }
        if part == ".." {
            return Err("远程路径不能包含 .. 段".into());
        }
        if part.chars().count() > 512 {
            return Err("远程路径段过长".into());
        }
        if parts.len() >= MAX_REMOTE_PATH_SEGMENTS {
            return Err("远程路径层级过深".into());
        }
        parts.push(part.to_string());
    }
    if parts.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

fn normalize_mutation_path(path: &str) -> Result<String, String> {
    let path = normalize_path(path)?;
    if path == "/" {
        Err("不允许对 WebDAV 根目录执行此操作".into())
    } else {
        Ok(path)
    }
}

fn checked_local_path(path: &str, label: &str) -> Result<PathBuf, String> {
    if path.chars().count() > MAX_LOCAL_PATH_CHARS
        || path
            .chars()
            .any(|character| character == '\0' || character.is_control())
    {
        return Err(format!("{label}路径过长或包含控制字符"));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(format!("{label}路径必须是绝对路径"));
    }
    Ok(path)
}

/// Open the exact upload object without following a final symlink, then keep
/// that handle alive through confirmation and streaming. This closes the
/// check/open race where a selected regular file could otherwise be replaced
/// with a link to a different local file after the user approved the upload.
fn open_upload_source(source: &std::path::Path) -> Result<(std::fs::File, u64), String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(source)
        .map_err(|error| format!("打开上传文件失败：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("读取上传文件失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("上传源必须是普通文件，不能是符号链接".into());
    }
    if metadata.len() > MAX_UPLOAD_BYTES {
        return Err("上传文件超过 512MB 限制".into());
    }
    Ok((file, metadata.len()))
}

#[tauri::command]
pub fn storage_providers_list() -> Result<Vec<StorageProviderConfig>, String> {
    Ok(read_store()?
        .providers
        .into_iter()
        .map(|mut p| {
            if p.password.as_deref().is_some_and(|v| !v.is_empty()) {
                p.password = Some(STORED_PASSWORD_MASK.into());
            }
            p
        })
        .collect())
}

fn apply_provider_upsert(
    store: &mut StorageConfigStore,
    mut config: StorageProviderConfig,
) -> Result<(), String> {
    if let Some(existing) = store.providers.iter_mut().find(|p| p.id == config.id) {
        if config.password.as_deref() == Some(STORED_PASSWORD_MASK) {
            if validate(existing).is_err()
                || target_url(existing, "/")? != target_url(&config, "/")?
                || existing.username.as_deref().unwrap_or_default()
                    != config.username.as_deref().unwrap_or_default()
            {
                return Err("WebDAV 地址或用户名已更改，请重新输入密码或清空密码后保存".into());
            }
            config.password = existing.password.clone();
        }
        *existing = config;
    } else {
        if store.providers.len() >= MAX_STORAGE_PROVIDERS {
            return Err(format!("存储源数量超过 {MAX_STORAGE_PROVIDERS} 个的上限"));
        }
        if config.password.as_deref() == Some(STORED_PASSWORD_MASK) {
            return Err("新存储源不能使用密码掩码，请重新输入密码".into());
        }
        store.providers.push(config);
    }
    Ok(())
}

fn upsert_provider_at(path: &Path, config: StorageProviderConfig) -> Result<(), String> {
    validate(&config)?;
    update_store_at(path, move |store| apply_provider_upsert(store, config))
}

fn remove_provider_at(path: &Path, id: &str) -> Result<(), String> {
    update_store_at(path, |store| {
        store.providers.retain(|provider| provider.id != id);
        Ok(())
    })
}

#[tauri::command]
pub fn storage_provider_upsert(config: StorageProviderConfig) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    upsert_provider_at(&config_path(), config)
}

#[tauri::command]
pub fn storage_provider_remove(id: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    remove_provider_at(&config_path(), &id)
}

async fn propfind(
    config: &StorageProviderConfig,
    path: &str,
    depth: &str,
) -> Result<reqwest::Response, String> {
    let method = reqwest::Method::from_bytes(b"PROPFIND").unwrap();
    request(config, method, path)?
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(PROPFIND_BODY)
        .send()
        .await
        .map_err(|e| format!("WebDAV 连接失败：{e}"))
}

#[tauri::command]
pub async fn storage_provider_test(id: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let response = propfind(&config, "/", "0").await?;
    if response.status().is_success() || response.status().as_u16() == 207 {
        Ok(())
    } else {
        Err(response_error(response, "连接测试").await)
    }
}

#[tauri::command]
pub async fn storage_list(id: String, path: String) -> Result<Vec<StorageEntry>, String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_path(&path)?;
    let response = propfind(&config, &path, "1").await?;
    if !(response.status().is_success() || response.status().as_u16() == 207) {
        return Err(response_error(response, "列出目录").await);
    }
    let xml = response_bytes_limited(response, MAX_PROPFIND_BODY_BYTES, "WebDAV 目录响应").await?;
    let xml = String::from_utf8(xml).map_err(|e| format!("WebDAV XML 不是有效 UTF-8：{e}"))?;
    parse_propfind(&xml, &config, &path)
}

#[tauri::command]
pub async fn storage_read_text(id: String, path: String) -> Result<String, String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_path(&path)?;
    let response = request(&config, reqwest::Method::GET, &path)?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(response_error(response, "读取文件").await);
    }
    let bytes = response_bytes_limited(response, MAX_TEXT_BODY_BYTES, "文本文件").await?;
    String::from_utf8(bytes).map_err(|e| format!("文件不是有效 UTF-8 文本：{e}"))
}

#[tauri::command]
pub async fn storage_write_text(id: String, path: String, content: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    put_text(&config, &path, content, false).await
}

#[tauri::command]
pub async fn storage_create_text(id: String, path: String, content: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    put_text(&config, &path, content, true).await
}

async fn put_text(
    config: &StorageProviderConfig,
    path: &str,
    content: String,
    create_only: bool,
) -> Result<(), String> {
    if content.len() > MAX_TEXT_BODY_BYTES {
        return Err("文本内容超过 5MB 限制".into());
    }
    let path = normalize_mutation_path(path)?;
    let mut request = request(config, reqwest::Method::PUT, &path)?
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(content);
    if create_only {
        request = request.header(reqwest::header::IF_NONE_MATCH, "*");
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    if create_only && status == reqwest::StatusCode::PRECONDITION_FAILED {
        Err("同名文件或文件夹已存在，请更换文件名".into())
    } else if create_only && status == reqwest::StatusCode::CREATED
        || !create_only && status.is_success()
    {
        Ok(())
    } else if create_only && status.is_success() {
        Err("WebDAV 服务器未确认新建文件，请刷新目录后检查".into())
    } else {
        Err(response_error(
            response,
            if create_only {
                "新建文件"
            } else {
                "写入文件"
            },
        )
        .await)
    }
}

#[tauri::command]
pub async fn storage_upload_file(
    app: AppHandle,
    id: String,
    path: String,
    local_path: String,
) -> Result<u64, String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_mutation_path(&path)?;
    let source = checked_local_path(&local_path, "上传源")?;
    let source_for_open = source.clone();
    let (file, size) =
        tauri::async_runtime::spawn_blocking(move || open_upload_source(&source_for_open))
            .await
            .map_err(|error| format!("打开上传文件的任务失败：{error}"))??;
    confirm_file_transfer(
        &app,
        "允许上传本地文件？",
        format!(
            "EchoAgent 将读取本地文件并上传到存储源。\n\n本地：{}\n远程：{}",
            source.display(),
            path
        ),
    )?;
    let file = tokio::fs::File::from_std(file);
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let response = request(&config, reqwest::Method::PUT, &path)?
        .header("Content-Type", "application/octet-stream")
        .header("Content-Length", size)
        .body(body)
        .send()
        .await
        .map_err(|e| format!("上传失败：{e}"))?;
    if response.status().is_success() {
        Ok(size)
    } else {
        Err(response_error(response, "上传文件").await)
    }
}

fn validate_download_destination(destination: &std::path::Path) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or("下载目标的父目录不存在")?;
    let file_name = destination.file_name().ok_or("下载目标必须是文件路径")?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("无法解析下载目录：{error}"))?;
    let destination = canonical_parent.join(file_name);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err("拒绝覆盖符号链接".into()),
        Ok(metadata) if !metadata.is_file() => Err("下载目标不是普通文件".into()),
        Ok(_) => Ok(destination),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(format!("无法检查下载目标：{error}")),
    }
}

#[tauri::command]
pub async fn storage_download_file(
    app: AppHandle,
    id: String,
    path: String,
    local_path: String,
) -> Result<u64, String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_path(&path)?;
    let requested_destination = checked_local_path(&local_path, "下载目标")?;
    let destination = validate_download_destination(&requested_destination)?;
    confirm_file_transfer(
        &app,
        "允许写入本地文件？",
        format!(
            "EchoAgent 将从存储源下载文件并写入以下位置：\n\n{}",
            destination.display()
        ),
    )?;
    let response = request(&config, reqwest::Method::GET, &path)?
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;
    if !response.status().is_success() {
        return Err(response_error(response, "下载文件").await);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        return Err("下载文件超过 512MB 限制".into());
    }

    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let temp = destination.with_file_name(format!(
        ".{file_name}.echoagent-{}.part",
        uuid::Uuid::now_v7()
    ));
    let result = async {
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
            .map_err(|e| format!("创建下载文件失败：{e}"))?;
        let mut stream = response.bytes_stream();
        let mut written = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("接收下载数据失败：{e}"))?;
            if written.saturating_add(chunk.len() as u64) > MAX_DOWNLOAD_BYTES {
                return Err("下载文件超过 512MB 限制".into());
            }
            output
                .write_all(&chunk)
                .await
                .map_err(|e| format!("写入下载文件失败：{e}"))?;
            written += chunk.len() as u64;
        }
        output
            .flush()
            .await
            .map_err(|e| format!("刷新下载文件失败：{e}"))?;
        output
            .sync_all()
            .await
            .map_err(|e| format!("同步下载文件失败：{e}"))?;
        drop(output);
        // Re-check immediately before replacing an existing path. In
        // particular, never rename a symlink out of the way and replace the
        // pathname as if the user had selected a regular file.
        let revalidated = validate_download_destination(&requested_destination)?;
        if revalidated != destination {
            return Err("下载目录在传输期间发生变化，已取消写入".into());
        }
        crate::paths::replace_file_atomically(&temp, &destination)
            .map_err(|error| format!("保存下载文件失败：{error}"))?;
        Ok::<u64, String>(written)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

#[tauri::command]
pub async fn storage_delete(id: String, path: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_mutation_path(&path)?;
    let response = request(&config, reqwest::Method::DELETE, &path)?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(response_error(response, "删除").await)
    }
}

#[tauri::command]
pub async fn storage_make_dir(id: String, path: String) -> Result<(), String> {
    crate::policy::require_feature("cloud-storage")?;
    let config = provider(&id)?;
    let path = normalize_mutation_path(&path)?;
    let method = reqwest::Method::from_bytes(b"MKCOL").unwrap();
    let response = request(&config, method, &path)?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(response_error(response, "创建目录").await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider(id: &str) -> StorageProviderConfig {
        StorageProviderConfig {
            id: id.into(),
            label: format!("Provider {id}"),
            kind: "webdav".into(),
            base_url: format!("https://dav.test/{id}"),
            username: Some("user".into()),
            password: Some(format!("secret-{id}")),
            enabled: true,
        }
    }

    fn read_store_with_lock_at(path: &Path) -> StorageConfigStore {
        with_store_access_at(path, || read_store_at(path)).unwrap()
    }

    #[test]
    fn parses_namespace_independent_propfind() {
        let config = StorageProviderConfig {
            id: "x".into(),
            label: "x".into(),
            kind: "webdav".into(),
            base_url: "https://dav.test/files/u".into(),
            username: None,
            password: None,
            enabled: true,
        };
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/files/u/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/files/u/a%20b.txt</d:href><d:propstat><d:prop><d:displayname>a b.txt</d:displayname><d:getcontentlength>12</d:getcontentlength><d:getcontenttype>text/plain</d:getcontenttype></d:prop></d:propstat></d:response></d:multistatus>"#;
        let entries = parse_propfind(xml, &config, "/").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/a b.txt");
        assert_eq!(entries[0].size, Some(12));
    }
    #[test]
    fn normalizes_safe_paths_and_rejects_traversal() {
        assert_eq!(normalize_path("/a//./b").unwrap(), "/a/b");
        assert_eq!(normalize_path("").unwrap(), "/");
        assert!(normalize_path("/a/../b").is_err());
        assert!(normalize_path("/a\n/b").is_err());
        assert!(normalize_mutation_path("/").is_err());
    }

    #[test]
    fn rejects_webdav_base_url_query_and_out_of_collection_entries() {
        let mut config = StorageProviderConfig {
            id: "x".into(),
            label: "x".into(),
            kind: "webdav".into(),
            base_url: "https://dav.test/files/u?secret=1".into(),
            username: None,
            password: None,
            enabled: true,
        };
        assert!(validate(&config).is_err());

        config.base_url = "https://dav.test/files/u".into();
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/files/user2/escape.txt</d:href><d:propstat><d:prop><d:displayname>escape.txt</d:displayname></d:prop></d:propstat></d:response></d:multistatus>"#;
        assert!(parse_propfind(xml, &config, "/").unwrap().is_empty());
    }

    #[test]
    fn rejects_plain_http_for_remote_webdav_credentials() {
        let config = StorageProviderConfig {
            id: "remote".into(),
            label: "remote".into(),
            kind: "webdav".into(),
            base_url: "http://dav.example.com/files".into(),
            username: Some("user".into()),
            password: Some("secret".into()),
            enabled: true,
        };
        assert!(validate(&config).is_err());
    }

    #[test]
    fn allows_plain_http_for_loopback_development_server() {
        let config = StorageProviderConfig {
            id: "local".into(),
            label: "local".into(),
            kind: "webdav".into(),
            base_url: "http://127.0.0.1:1900/dav".into(),
            username: None,
            password: None,
            enabled: true,
        };
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn masked_password_remains_bound_to_the_saved_endpoint_and_account() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        let mut saved = test_provider("saved");
        saved.base_url = "https://dav.test/export".into();
        upsert_provider_at(&path, saved.clone()).unwrap();

        let mut masked = saved.clone();
        masked.password = Some(STORED_PASSWORD_MASK.into());
        masked.base_url = "https://DAV.test:443/export/".into();
        masked.label = "新显示名".into();
        upsert_provider_at(&path, masked.clone()).unwrap();
        let persisted = read_store_with_lock_at(&path).providers.remove(0);
        assert_eq!(persisted.password, saved.password);
        assert_eq!(persisted.label, masked.label);
        let bytes = std::fs::read(&path).unwrap();

        for endpoint in [
            "https://other.test/export",
            "https://dav.test:8443/export",
            "https://dav.test/another-collection",
        ] {
            let mut edited = masked.clone();
            edited.base_url = endpoint.into();
            let error = upsert_provider_at(&path, edited).unwrap_err();
            assert!(error.contains("重新输入密码"));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let mut other_user = masked.clone();
        other_user.username = Some("other".into());
        assert!(upsert_provider_at(&path, other_user).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        masked.base_url = "https://other.test/export".into();
        masked.password = Some("replacement-test-value".into());
        upsert_provider_at(&path, masked.clone()).unwrap();
        let persisted = read_store_with_lock_at(&path).providers.remove(0);
        assert_eq!(persisted.base_url, masked.base_url);
        assert_eq!(persisted.password, masked.password);

        masked.base_url = "https://dav.test/anonymous".into();
        masked.password = None;
        upsert_provider_at(&path, masked).unwrap();
        assert!(read_store_with_lock_at(&path).providers[0]
            .password
            .is_none());
    }

    #[tokio::test]
    async fn conditional_text_creation_handles_conflicts_and_keeps_editing_available() {
        use axum::{body::Bytes, extract::State, http::HeaderMap, routing::put, Router};
        use reqwest::StatusCode;
        use std::sync::Arc;

        let contents = Arc::new(Mutex::new(None::<Bytes>));
        let route = put(
            |State(contents): State<Arc<Mutex<Option<Bytes>>>>, headers: HeaderMap, body: Bytes| async move {
                let mut content = contents.lock().unwrap();
                if headers
                    .get(reqwest::header::IF_NONE_MATCH)
                    .is_some_and(|v| v == "*")
                    && content.is_some()
                {
                    return StatusCode::PRECONDITION_FAILED;
                }
                let existed = content.replace(body).is_some();
                if existed {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::CREATED
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = StorageProviderConfig {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            username: None,
            password: None,
            ..test_provider("isolated")
        };
        let router = Router::new()
            .route("/note.txt", route)
            .with_state(contents.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let (first, second) = tokio::join!(
            put_text(&config, "/note.txt", "first version".into(), true),
            put_text(&config, "/note.txt", "second version".into(), true),
        );
        let expected = match (first, second) {
            (Ok(()), Err(error)) if error.contains("同名") => "first version",
            (Err(error), Ok(())) if error.contains("同名") => "second version",
            result => panic!("Expected one creation and one conflict, got {result:?}"),
        };
        assert_eq!(
            contents.lock().unwrap().as_deref(),
            Some(expected.as_bytes())
        );
        put_text(&config, "/note.txt", "edited".into(), false)
            .await
            .unwrap();
        assert_eq!(
            contents.lock().unwrap().as_deref(),
            Some("edited".as_bytes())
        );
        server.abort();
    }

    #[test]
    fn storage_transaction_oversized_write_preserves_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        write_store_at(
            &path,
            &StorageConfigStore {
                providers: vec![test_provider("existing")],
            },
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();

        let error = update_store_at(&path, |store| {
            store.providers = (0..MAX_STORAGE_PROVIDERS)
                .map(|index| {
                    let mut provider = test_provider(&format!("large-{index}"));
                    provider.password = Some("x".repeat(40 * 1024));
                    provider
                })
                .collect();
            Ok(())
        })
        .unwrap_err();

        assert!(error.contains("2MB"), "unexpected error: {error}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn storage_transaction_corrupt_file_is_quarantined_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        let corrupt = b"{ definitely not valid JSON";
        std::fs::write(&path, corrupt).unwrap();

        let error = upsert_provider_at(&path, test_provider("new")).unwrap_err();

        assert!(error.contains("JSON 损坏"), "unexpected error: {error}");
        assert!(
            !path.exists(),
            "mutation must not replace quarantined input"
        );
        let backups = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("providers.json.corrupt.")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), corrupt);
    }

    #[cfg(unix)]
    #[test]
    fn storage_transaction_symlink_is_not_followed_or_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.json");
        let path = temp.path().join("providers.json");
        let outside = b"outside data must survive";
        std::fs::write(&target, outside).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let error = remove_provider_at(&path, "anything").unwrap_err();

        assert!(error.contains("符号链接"), "unexpected error: {error}");
        assert_eq!(std::fs::read(&target).unwrap(), outside);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn storage_transaction_concurrent_upserts_and_removals_preserve_changes() {
        const CHANGES: usize = 8;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        let mut initial = vec![test_provider("keeper")];
        initial.extend((0..CHANGES).map(|index| test_provider(&format!("old-{index}"))));
        write_store_at(&path, &StorageConfigStore { providers: initial }).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(CHANGES * 2));
        let mut handles = Vec::new();
        for index in 0..CHANGES {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                upsert_provider_at(&path, test_provider(&format!("new-{index}")))
            }));
        }
        for index in 0..CHANGES {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                remove_provider_at(&path, &format!("old-{index}"))
            }));
        }
        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let store = read_store_with_lock_at(&path);
        let ids = store
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), CHANGES + 1);
        assert!(ids.contains("keeper"));
        for index in 0..CHANGES {
            assert!(ids.contains(format!("new-{index}").as_str()));
            assert!(!ids.contains(format!("old-{index}").as_str()));
        }
    }

    const CHILD_PATH_ENV: &str = "ECHOAGENT_STORAGE_TRANSACTION_CHILD_PATH";
    const CHILD_ID_ENV: &str = "ECHOAGENT_STORAGE_TRANSACTION_CHILD_ID";
    const CHILD_READY_ENV: &str = "ECHOAGENT_STORAGE_TRANSACTION_CHILD_READY";
    const STORAGE_RELEASE_BYTE: u8 = b'S';
    const STORAGE_CHILD_PHASE_TIMEOUT: Duration = Duration::from_secs(10);

    fn read_storage_release(mut input: impl std::io::Read) -> std::io::Result<()> {
        let mut release = [0_u8; 1];
        input.read_exact(&mut release)?;
        if release[0] != STORAGE_RELEASE_BYTE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid storage writer release byte",
            ));
        }
        Ok(())
    }

    struct StorageTestWriter {
        child: std::process::Child,
        stdout: PathBuf,
        stderr: PathBuf,
    }

    #[derive(Default)]
    struct StorageTestWriters {
        writers: Vec<StorageTestWriter>,
    }

    impl Drop for StorageTestWriters {
        fn drop(&mut self) {
            // Close every release pipe first, including when startup panics.
            // A waiting child observes EOF instead of a stale start marker.
            for writer in &mut self.writers {
                drop(writer.child.stdin.take());
            }
            for writer in &mut self.writers {
                if !matches!(writer.child.try_wait(), Ok(Some(_))) {
                    let _ = writer.child.kill();
                }
            }
            // Cleanup must not introduce an unbounded wait after a deadline.
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            loop {
                let mut all_exited = true;
                for writer in &mut self.writers {
                    all_exited &= matches!(writer.child.try_wait(), Ok(Some(_)));
                }
                if all_exited || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn storage_child_diagnostics(writer: &StorageTestWriter) -> String {
        use std::io::Read;
        let read = |path: &Path| {
            let mut text = String::new();
            if let Ok(file) = std::fs::File::open(path) {
                let _ = file.take(64 * 1024).read_to_string(&mut text);
            }
            text
        };
        format!(
            "stdout:\n{}\nstderr:\n{}",
            read(&writer.stdout),
            read(&writer.stderr)
        )
    }

    fn spawn_storage_test_writer(
        writers: &mut StorageTestWriters,
        path: &Path,
        ready: &Path,
        id: &str,
    ) {
        let spawn_started = std::time::Instant::now();
        let stdout = ready.with_extension("stdout");
        let stderr = ready.with_extension("stderr");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("storage::tests::storage_transaction_child_writer")
            .arg("--nocapture")
            .env(CHILD_PATH_ENV, path)
            .env(CHILD_ID_ENV, id)
            .env(CHILD_READY_ENV, ready)
            .stdin(std::process::Stdio::piped())
            // Files avoid output-pipe backpressure and an unbounded
            // wait_with_output/read_to_end after the child exits.
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        // On a failed full-suite run the harness previously discarded all
        // spawn timings. Preserve actual startup evidence without changing the
        // ten-second barrier, release protocol or concurrent-write assertions.
        eprintln!(
            "storage test child {id}: pid={}, spawn={:?}, ready={}",
            child.id(),
            spawn_started.elapsed(),
            ready.exists()
        );
        writers.writers.push(StorageTestWriter {
            child,
            stdout,
            stderr,
        });
    }

    fn storage_children_alive(writers: &mut StorageTestWriters) -> Result<(), String> {
        for writer in &mut writers.writers {
            if let Some(status) = writer.child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!(
                    "storage child exited before release ({status}):\n{}",
                    storage_child_diagnostics(writer)
                ));
            }
        }
        Ok(())
    }

    fn wait_for_storage_children_ready(
        writers: &mut StorageTestWriters,
        ready_paths: &[PathBuf],
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("child readiness barrier timed out".into());
            }
            storage_children_alive(writers)?;
            if ready_paths.iter().all(|ready| ready.exists()) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn release_storage_children(writers: &mut StorageTestWriters) -> Result<(), String> {
        use std::io::Write;
        storage_children_alive(writers)?;
        // Release all four ready writers before waiting for any transaction.
        for writer in &mut writers.writers {
            let mut pipe = writer
                .child
                .stdin
                .take()
                .ok_or("storage release pipe missing")?;
            pipe.write_all(&[STORAGE_RELEASE_BYTE])
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn wait_for_storage_children_complete(
        writers: &mut StorageTestWriters,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        loop {
            let mut all_exited = true;
            for writer in &mut writers.writers {
                match writer.child.try_wait().map_err(|error| error.to_string())? {
                    Some(status) if !status.success() => {
                        return Err(format!(
                            "storage child failed ({status}):\n{}",
                            storage_child_diagnostics(writer)
                        ));
                    }
                    Some(_) => {}
                    None => all_exited = false,
                }
            }
            if all_exited {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("storage transaction completion timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Invoked by `storage_transaction_cross_process_upserts_preserve_changes`
    /// through the Rust test harness. A normal test-suite invocation has no
    /// path environment variable and returns immediately.
    #[test]
    fn storage_transaction_child_writer() {
        let Some(path) = std::env::var_os(CHILD_PATH_ENV).map(PathBuf::from) else {
            return;
        };
        let id = std::env::var(CHILD_ID_ENV).expect("child id");
        let ready = PathBuf::from(std::env::var_os(CHILD_READY_ENV).expect("ready path"));
        std::fs::write(&ready, b"ready").unwrap();
        // The parent owns both phase deadlines. EOF or a wrong token fails
        // before any validation or store transaction; no earlier child's
        // independent countdown can expire while its siblings start up.
        read_storage_release(std::io::stdin().lock()).expect("storage release pipe failed");

        let config = test_provider(&id);
        validate(&config).unwrap();
        update_store_at(&path, move |store| {
            // Deterministically widens the stale-snapshot window. The file
            // lock keeps other processes outside the transaction during it.
            std::thread::sleep(Duration::from_millis(100));
            apply_provider_upsert(store, config)
        })
        .unwrap();
    }

    #[test]
    fn storage_transaction_cross_process_upserts_preserve_changes() {
        const CHILDREN: usize = 4;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        write_store_at(
            &path,
            &StorageConfigStore {
                providers: vec![test_provider("keeper")],
            },
        )
        .unwrap();

        let mut children = StorageTestWriters::default();
        let ready_paths: Vec<_> = (0..CHILDREN)
            .map(|index| temp.path().join(format!("ready-{index}")))
            .collect();
        let ready_deadline = std::time::Instant::now() + STORAGE_CHILD_PHASE_TIMEOUT;
        // This exercises four concurrent processes, not four sequential OS
        // launches. A slow spawn must not consume each following writer's
        // shared startup budget before that writer has even been launched.
        std::thread::scope(|scope| {
            let handles: Vec<_> = ready_paths
                .iter()
                .enumerate()
                .map(|(index, ready)| {
                    let path = &path;
                    scope.spawn(move || {
                        let mut group = StorageTestWriters::default();
                        assert!(
                            std::time::Instant::now() < ready_deadline,
                            "child startup timed out"
                        );
                        spawn_storage_test_writer(
                            &mut group,
                            path,
                            ready,
                            &format!("child-{index}"),
                        );
                        // The group owns cleanup even if this worker panics.
                        group
                    })
                })
                .collect();
            let mut startup_failed = false;
            for handle in handles {
                match handle.join() {
                    Ok(mut group) => children.writers.append(&mut group.writers),
                    Err(_) => startup_failed = true,
                }
            }
            assert!(!startup_failed, "storage writer startup failed");
        });
        assert_eq!(children.writers.len(), CHILDREN);
        wait_for_storage_children_ready(&mut children, &ready_paths, ready_deadline).unwrap();
        assert!(std::time::Instant::now() < ready_deadline);
        release_storage_children(&mut children).unwrap();
        wait_for_storage_children_complete(
            &mut children,
            std::time::Instant::now() + STORAGE_CHILD_PHASE_TIMEOUT,
        )
        .unwrap();

        let store = read_store_with_lock_at(&path);
        assert_eq!(store.providers.len(), CHILDREN + 1);
        assert!(store
            .providers
            .iter()
            .any(|provider| provider.id == "keeper"));
        for index in 0..CHILDREN {
            assert!(store
                .providers
                .iter()
                .any(|provider| provider.id == format!("child-{index}")));
        }
    }

    fn assert_storage_release_failure_preserves_store(release: Option<u8>) {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("providers.json");
        let ready = temp.path().join("ready");
        write_store_at(
            &path,
            &StorageConfigStore {
                providers: vec![test_provider("keeper")],
            },
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut children = StorageTestWriters::default();
        let deadline = std::time::Instant::now() + STORAGE_CHILD_PHASE_TIMEOUT;
        spawn_storage_test_writer(&mut children, &path, &ready, "must-not-be-written");
        wait_for_storage_children_ready(&mut children, std::slice::from_ref(&ready), deadline)
            .unwrap();
        let mut pipe = children.writers[0].child.stdin.take().unwrap();
        if let Some(release) = release {
            pipe.write_all(&[release]).unwrap();
        }
        drop(pipe);
        let failure = wait_for_storage_children_complete(
            &mut children,
            std::time::Instant::now() + STORAGE_CHILD_PHASE_TIMEOUT,
        )
        .unwrap_err();
        assert!(failure.contains("storage release pipe failed"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let store = read_store_with_lock_at(&path);
        assert_eq!(store.providers.len(), 1);
        assert_eq!(store.providers[0].id, "keeper");
        assert!(ready.exists());
        // A persistent ready marker cannot authorize a dead writer, the exact
        // stale-marker condition accepted by the old startup barrier.
        assert!(wait_for_storage_children_ready(
            &mut children,
            &[ready],
            std::time::Instant::now() + STORAGE_CHILD_PHASE_TIMEOUT,
        )
        .unwrap_err()
        .contains("exited before release"));
    }

    #[test]
    fn storage_transaction_release_pipe_eof_preserves_store() {
        assert_storage_release_failure_preserves_store(None);
    }

    #[test]
    fn storage_transaction_release_pipe_rejects_invalid_token_without_updates() {
        assert_storage_release_failure_preserves_store(Some(b'X'));
    }

    // Included only inside storage::tests. The companion standard server script
    // creates synthetic credentials and an isolated process-level ECHO_AGENT_HOME.
    #[tokio::test]
    #[ignore = "requires the real WsgiDAV/Cheroot resources from scripts/standard-webdav-review.py"]
    async fn webdav_standard_server_real_commands() {
        use sha2::{Digest, Sha256};
        let manifest_path = PathBuf::from(
            std::env::var_os("ECHO_WEBDAV_REVIEW_MANIFEST")
                .expect("real WebDAV manifest is required"),
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema"], 1);
        let text = |key: &str| {
            manifest[key]
                .as_str()
                .expect("manifest string is required")
                .to_owned()
        };
        let home = PathBuf::from(text("home"));
        let root = PathBuf::from(text("root"));
        let run = manifest_path.parent().unwrap().canonicalize().unwrap();
        assert_eq!(run.parent().unwrap(), Path::new("/private/tmp"));
        assert!(run
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("echo-webdav-real-"));
        assert_eq!(home.canonicalize().unwrap(), run.join("echo-home"));
        assert_eq!(root.canonicalize().unwrap(), run.join("physical-root"));
        assert_eq!(
            crate::paths::echo_agent_home_dir().canonicalize().unwrap(),
            home
        );
        let phase =
            std::env::var("ECHO_WEBDAV_REVIEW_PHASE").expect("first/restart phase is required");
        assert!(["first", "restart"].contains(&phase.as_str()));
        let output = PathBuf::from(
            std::env::var_os("ECHO_WEBDAV_REVIEW_OUTPUT").expect("output is required"),
        );
        let seed_name = text("seedName");
        assert_eq!(seed_name, "原始 中文.txt");
        let seed_content = text("seedContent");
        let marker = "真实 Rust WebDAV 中文验收\n保留空格、表格 | 列一 | 列二 |\n";
        let directory = "本轮验收 有空格";
        let filename = "恢复 中文.txt";
        let remote = format!("/{directory}/{filename}");
        let physical = root.join("rw").join(directory).join(filename);
        let mut checks = Vec::<serde_json::Value>::new();
        let mut record = |id: &str, evidence: serde_json::Value| {
            checks.push(serde_json::json!({"id":id,"passed":true,"evidence":evidence}));
        };
        record(
            "isolated-real-home-and-physical-root",
            serde_json::json!({"phase":phase}),
        );
        let provider_config = |id: &str, endpoint: &str| {
            let base_url = text(endpoint);
            let url = url::Url::parse(&base_url).unwrap();
            assert_eq!(url.scheme(), "http");
            assert_eq!(url.host_str(), Some("127.0.0.1"));
            assert!(url.port().is_some());
            assert!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            );
            StorageProviderConfig {
                id: id.into(),
                label: "真实本地 WebDAV 验收".into(),
                kind: "webdav".into(),
                base_url,
                username: Some(text("username")),
                password: Some(text("password")),
                enabled: true,
            }
        };
        let mut rw = provider_config("review-rw", "readWriteUrl");
        let ro = provider_config("review-ro", "readOnlyUrl");
        if phase == "first" {
            assert!(storage_providers_list().unwrap().is_empty());
            storage_provider_upsert(rw.clone()).unwrap();
            storage_provider_upsert(ro.clone()).unwrap();
            let mut unauthorized = rw.clone();
            unauthorized.id = "review-unauthorized".into();
            unauthorized.password = Some("deliberately-wrong".into());
            storage_provider_upsert(unauthorized).unwrap();
            assert_eq!(storage_providers_list().unwrap().len(), 3);
            assert!(storage_providers_list()
                .unwrap()
                .iter()
                .all(|entry| entry.password.as_deref() == Some("••••")));
            record(
                "provider-create-persist-and-mask",
                serde_json::json!({"providers":3}),
            );
            rw.password = Some("••••".into());
            storage_provider_upsert(rw.clone()).unwrap();
            storage_provider_test(rw.id.clone()).await.unwrap();
            storage_provider_test(ro.id.clone()).await.unwrap();
            record(
                "real-propfind-connect-and-masked-password-preservation",
                serde_json::json!({"shares":2}),
            );
            let entries = storage_list(rw.id.clone(), "/".into()).await.unwrap();
            let seed = entries
                .iter()
                .find(|entry| entry.name == seed_name)
                .expect("Chinese spaced seed must be listed");
            assert_eq!(seed.path, format!("/{seed_name}"));
            assert!(!seed.is_dir);
            assert_eq!(seed.size, Some(seed_content.len() as u64));
            record(
                "real-propfind-xml-and-chinese-spaced-path",
                serde_json::json!({"size":seed.size}),
            );
            assert_eq!(
                storage_read_text(rw.id.clone(), format!("/{seed_name}"))
                    .await
                    .unwrap(),
                seed_content
            );
            assert_eq!(
                storage_read_text(ro.id.clone(), format!("/{seed_name}"))
                    .await
                    .unwrap(),
                seed_content
            );
            record(
                "real-get-utf8-from-both-shares",
                serde_json::json!({"size":seed_content.len()}),
            );
            let unauthorized = storage_provider_test("review-unauthorized".into())
                .await
                .unwrap_err();
            assert!(unauthorized.contains("401"));
            record(
                "wrong-credential-401-is-error",
                serde_json::json!({"status":401}),
            );
            storage_make_dir(rw.id.clone(), format!("/{directory}"))
                .await
                .unwrap();
            assert!(physical.parent().unwrap().is_dir());
            record(
                "real-mkcol-physical-directory",
                serde_json::json!({"name":directory}),
            );
            storage_write_text(rw.id.clone(), remote.clone(), marker.into())
                .await
                .unwrap();
            let bytes = std::fs::read(&physical).unwrap();
            assert_eq!(bytes, marker.as_bytes());
            assert_eq!(
                storage_read_text(rw.id.clone(), remote.clone())
                    .await
                    .unwrap(),
                marker
            );
            record(
                "real-put-physical-bytes-and-get-roundtrip",
                serde_json::json!({"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}),
            );
            let entries = storage_list(rw.id.clone(), format!("/{directory}"))
                .await
                .unwrap();
            assert!(entries
                .iter()
                .any(|entry| entry.path == remote && entry.name == filename));
            record(
                "nested-directory-list-keeps-relative-path",
                serde_json::json!({"path":remote}),
            );
            let denied = storage_write_text(
                ro.id.clone(),
                format!("/{seed_name}"),
                "must not be written".into(),
            )
            .await
            .unwrap_err();
            assert!(denied.contains("403"));
            assert_eq!(
                std::fs::read(root.join("ro").join(&seed_name)).unwrap(),
                seed_content.as_bytes()
            );
            record(
                "read-only-put-403-preserves-file",
                serde_json::json!({"status":403}),
            );
            assert!(storage_delete(rw.id.clone(), "/".into())
                .await
                .unwrap_err()
                .contains("根目录"));
            assert!(storage_write_text(
                rw.id.clone(),
                "/../escape.txt".into(),
                "must not be written".into()
            )
            .await
            .is_err());
            assert!(!run.join("escape.txt").exists());
            record("root-delete-and-traversal-refused", serde_json::json!({}));
            rw.enabled = false;
            storage_provider_upsert(rw.clone()).unwrap();
            assert!(storage_read_text(rw.id.clone(), remote.clone())
                .await
                .unwrap_err()
                .contains("未启用"));
            assert_eq!(std::fs::read(&physical).unwrap(), marker.as_bytes());
            record(
                "disabled-provider-blocks-use-preserves-file",
                serde_json::json!({}),
            );
            rw.enabled = true;
            storage_provider_upsert(rw.clone()).unwrap();
            assert_eq!(
                storage_read_text(rw.id.clone(), remote.clone())
                    .await
                    .unwrap(),
                marker
            );
            record(
                "reenable-recovers-with-preserved-secret",
                serde_json::json!({}),
            );
            assert!(storage_read_text(rw.id.clone(), "/不存在 中文.txt".into())
                .await
                .unwrap_err()
                .contains("404"));
            record(
                "missing-file-404-is-error",
                serde_json::json!({"status":404}),
            );
            let scratch = format!("/{directory}/只删本轮文件.txt");
            storage_write_text(rw.id.clone(), scratch.clone(), "scratch".into())
                .await
                .unwrap();
            storage_delete(rw.id.clone(), scratch).await.unwrap();
            assert!(!physical.parent().unwrap().join("只删本轮文件.txt").exists());
            assert_eq!(std::fs::read(&physical).unwrap(), marker.as_bytes());
            record("real-delete-own-scratch-only", serde_json::json!({}));
            assert!(config_path().is_file());
            assert_eq!(read_store().unwrap().providers.len(), 3);
            record(
                "provider-config-and-files-left-for-fresh-process",
                serde_json::json!({"providers":3}),
            );
        } else {
            assert_eq!(storage_providers_list().unwrap().len(), 3);
            storage_provider_test(rw.id.clone()).await.unwrap();
            assert_eq!(
                storage_read_text(rw.id.clone(), remote.clone())
                    .await
                    .unwrap(),
                marker
            );
            assert_eq!(std::fs::read(&physical).unwrap(), marker.as_bytes());
            assert!(storage_list(rw.id.clone(), format!("/{directory}"))
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.path == remote));
            record(
                "fresh-rust-process-reloads-provider-and-chinese-file",
                serde_json::json!({"path":remote,"size":marker.len()}),
            );
            for id in ["review-unauthorized", "review-ro", "review-rw"] {
                storage_provider_remove(id.into()).unwrap();
            }
            assert!(storage_providers_list().unwrap().is_empty());
            let revoked = storage_read_text(rw.id.clone(), remote.clone())
                .await
                .unwrap_err();
            assert!(revoked.contains("不存在或未启用"));
            assert_eq!(std::fs::read(&physical).unwrap(), marker.as_bytes());
            assert!(read_store().unwrap().providers.is_empty());
            record(
                "revocation-persists-blocks-read-does-not-delete-remote-file",
                serde_json::json!({"providers":0}),
            );
        }
        for share in ["rw", "ro"] {
            assert_eq!(
                std::fs::read(root.join(share).join(&seed_name)).unwrap(),
                seed_content.as_bytes()
            );
        }
        record(
            "original-synthetic-seeds-preserved",
            serde_json::json!({"shares":2}),
        );
        let evidence = serde_json::json!({"passed":true,"phase":phase,"layer":"real-production-rust-storage-commands",
        "checks":checks,"transferDialogVerified":false,"nativeCloudPanelVerified":false});
        std::fs::write(output, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
