//! Personal Weixin remote control. Transport stays in the native process so a
//! hidden desktop window does not interrupt delivery. The channel only routes
//! to the existing session and interaction services; it owns no Agent state.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Listener, Manager, State};
use url::Url;
use uuid::Uuid;

use crate::bridge::{CompleteEvent, PermissionFrontend, QuestionFrontend};
use crate::commands::AppState;
use crate::sessions::SessionSummary;

const API_BASE: &str = "https://ilinkai.weixin.qq.com";
const PROTOCOL_VERSION: &str = "2.4.9";
const CREDENTIAL_ACCOUNT: &str = "echoagent-weixin-personal";
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REPLY_CHARS: usize = 3_600;
const MAX_INPUT_CHARS: usize = 32_000;
const TASKS_PER_PAGE: usize = 5;
const MAX_SEEN: usize = 256;
const MAX_INFLIGHT: usize = 64;
const MAX_ROUTES: usize = 256;
const MAX_OUTBOX: usize = 256;
static BINDING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static OUTBOX_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static OUTBOX_NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Binding {
    bot_id: String,
    user_id: String,
    base_url: String,
    active_session: Option<String>,
    allowed_workspaces: Vec<String>,
    default_workspace: Option<String>,
    shared_sessions: Vec<String>,
    cursor: String,
    context_token: Option<String>,
    seen_ids: VecDeque<String>,
    inflight: VecDeque<InboundAttempt>,
    outbound_routes: VecDeque<(String, String)>,
    outbound_pending: VecDeque<(String, String)>,
    outbox: VecDeque<OutboundText>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutboundText {
    client_id: String,
    text: String,
    session_id: Option<String>,
    context_token: Option<String>,
    request_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboundAttempt {
    message_id: String,
    description: String,
}

#[derive(Clone, Debug)]
struct Login {
    qrcode: String,
    base_url: String,
    created: Instant,
}

#[derive(Clone, Debug)]
enum PendingKind {
    Permission {
        allow: Option<String>,
        deny: Option<String>,
    },
    Question {
        questions: Vec<String>,
    },
    Plan {
        can_approve: bool,
    },
}

#[derive(Clone, Debug)]
struct Pending {
    request_id: String,
    session_id: String,
    kind: PendingKind,
}

#[derive(Default)]
pub(crate) struct WeixinState {
    login: Mutex<Option<Login>>,
    login_generation: AtomicU64,
    worker: Mutex<Option<JoinHandle<()>>>,
    outbox_worker: Mutex<Option<JoinHandle<()>>>,
    worker_generation: AtomicU64,
    worker_alive: AtomicBool,
    outbox_alive: AtomicBool,
    output: Mutex<HashMap<String, String>>,
    pending: Mutex<HashMap<String, Pending>>,
    progress_at: Mutex<HashMap<String, Instant>>,
    last_success: Mutex<Option<SystemTime>>,
    last_error: Mutex<Option<String>>,
    outbox_error: Mutex<Option<String>>,
    paused_until: Mutex<Option<Instant>>,
}

struct WorkerGuard {
    app: AppHandle,
    generation: u64,
    outbox: bool,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let state = self.app.state::<WeixinState>();
        if state.worker_generation.load(Ordering::SeqCst) == self.generation {
            if self.outbox {
                state.outbox_alive.store(false, Ordering::SeqCst);
            } else {
                state.worker_alive.store(false, Ordering::SeqCst);
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WeixinStatus {
    connected: bool,
    bot_id: Option<String>,
    active_session: Option<String>,
    active_session_title: Option<String>,
    allowed_workspaces: Vec<String>,
    default_workspace: Option<String>,
    shared_sessions: Vec<SharedSession>,
    pending_replies: usize,
    online: bool,
    last_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedSession {
    session_id: String,
    title: String,
    cwd: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QrStart {
    qr_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QrPoll {
    status: String,
    connected: bool,
}

fn state_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("weixin-channel.json")
}

fn load_binding() -> Result<Option<Binding>, String> {
    let path = state_path();
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取微信通道配置失败：{error}")),
    }
    let raw = crate::shell_fs::read_regular_file_bounded(&path, MAX_STATE_BYTES)?;
    let binding: Binding =
        serde_json::from_slice(&raw).map_err(|error| format!("微信通道配置无效：{error}"))?;
    if binding.bot_id.is_empty() || binding.user_id.is_empty() {
        return Err("微信通道配置缺少绑定身份".into());
    }
    validate_base_url(&binding.base_url)?;
    Ok(Some(binding))
}

fn save_binding(binding: &Binding) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(binding)
        .map_err(|error| format!("序列化微信通道配置失败：{error}"))?;
    if raw.len() as u64 > MAX_STATE_BYTES {
        return Err("微信通道配置超出容量限制".into());
    }
    crate::paths::write_private_file(&state_path(), &raw)
}

fn mutate_binding<F>(change: F) -> Result<Binding, String>
where
    F: FnOnce(&mut Binding) -> Result<(), String>,
{
    let _guard = BINDING_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let mut binding = load_binding()?.ok_or("请先绑定微信")?;
    change(&mut binding)?;
    save_binding(&binding)?;
    Ok(binding)
}

fn validate_base_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "微信服务器地址无效".to_string())?;
    let host = url.host_str().ok_or("微信服务器地址缺少主机名")?;
    if url.scheme() != "https"
        || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("微信服务器地址不在允许的 HTTPS 域名内".into());
    }
    Ok(url)
}

fn api_url(base: &str, path: &str) -> Result<Url, String> {
    validate_base_url(base)?
        .join(path)
        .map_err(|_| "无法构造微信接口地址".to_string())
}

fn api_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("初始化微信连接失败：{error}"))
}

fn client_version() -> u32 {
    // Weixin's protocol uses a 0x00MMNNPP channel-version header.
    (2 << 16) | (4 << 8) | 9
}

fn uin_header() -> String {
    let value = Uuid::now_v7().as_u128() as u32;
    base64::engine::general_purpose::STANDARD.encode(value.to_string())
}

async fn api_post(
    base: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let url = api_url(base, path)?;
    let mut request = api_client(timeout)?
        .post(url)
        .header("AuthorizationType", "ilink_bot_token")
        .header("X-WECHAT-UIN", uin_header())
        .header("iLink-App-Id", "bot")
        .header("iLink-App-ClientVersion", client_version().to_string())
        .json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "微信连接超时".into()
        } else {
            format!("微信连接失败：{error}")
        }
    })?;
    if !response.status().is_success() {
        return Err(format!("微信接口返回 HTTP {}", response.status()));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| "微信接口返回格式无效".to_string())?;
    let ret = value.get("ret").and_then(Value::as_i64).unwrap_or(0);
    let errcode = value.get("errcode").and_then(Value::as_i64).unwrap_or(0);
    if ret != 0 || errcode != 0 {
        return Err(format!(
            "微信接口返回错误 {}",
            if errcode != 0 { errcode } else { ret }
        ));
    }
    Ok(value)
}

async fn api_get(base: &str, path: &str, timeout: Duration) -> Result<Value, String> {
    let url = api_url(base, path)?;
    let response = api_client(timeout)?
        .get(url)
        .header("iLink-App-Id", "bot")
        .header("iLink-App-ClientVersion", client_version().to_string())
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "微信扫码状态检查超时".into()
            } else {
                format!("微信扫码状态检查失败：{error}")
            }
        })?;
    if !response.status().is_success() {
        return Err(format!("微信扫码状态接口返回 HTTP {}", response.status()));
    }
    response
        .json()
        .await
        .map_err(|_| "微信扫码状态格式无效".to_string())
}

fn base_info() -> Value {
    json!({"channel_version": PROTOCOL_VERSION, "bot_agent": format!("EchoAgent/{}", env!("CARGO_PKG_VERSION"))})
}

fn cdn_url(media: &Value) -> Result<Url, String> {
    let raw = if let Some(full) = media
        .get("full_url")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        full.to_string()
    } else {
        let query = media
            .get("encrypt_query_param")
            .and_then(Value::as_str)
            .ok_or("附件缺少下载地址")?;
        format!(
            "https://novac2c.cdn.weixin.qq.com/c2c/download?encrypted_query_param={}",
            urlencoding::encode(query)
        )
    };
    let url = Url::parse(&raw).map_err(|_| "微信附件地址无效")?;
    let host = url.host_str().ok_or("微信附件地址缺少主机名")?;
    if url.scheme() != "https"
        || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("微信附件下载地址不在允许的域名内".into());
    }
    Ok(url)
}

// cipher 0.4's in-place block API uses generic-array 0.14 internally.
#[allow(deprecated)]
fn decrypt_media(mut bytes: Vec<u8>, raw_key: &str, hex_key: bool) -> Result<Vec<u8>, String> {
    let decoded = if hex_key {
        hex::decode(raw_key).map_err(|_| "微信附件密钥无效")?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(raw_key)
            .map_err(|_| "微信附件密钥无效")?
    };
    let key = if decoded.len() == 32 && decoded.iter().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(&decoded).map_err(|_| "微信附件密钥无效")?
    } else {
        decoded
    };
    if key.len() != 16 || bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        return Err("微信附件加密格式无效".into());
    }
    let cipher = aes::Aes128::new_from_slice(&key).map_err(|_| "微信附件密钥无效")?;
    for block in bytes.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            block,
        ));
    }
    let padding = *bytes.last().unwrap() as usize;
    if padding == 0
        || padding > 16
        || !bytes[bytes.len() - padding..]
            .iter()
            .all(|b| *b as usize == padding)
    {
        return Err("微信附件解密校验失败".into());
    }
    bytes.truncate(bytes.len() - padding);
    Ok(bytes)
}

#[allow(deprecated)]
fn encrypt_media(mut bytes: Vec<u8>, key: &[u8; 16]) -> Result<Vec<u8>, String> {
    let padding = 16 - bytes.len() % 16;
    bytes.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher = aes::Aes128::new_from_slice(key).map_err(|_| "微信附件密钥无效")?;
    for block in bytes.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            block,
        ));
    }
    Ok(bytes)
}

async fn send_workspace_file(
    app: &AppHandle,
    binding: &Binding,
    session_id: &str,
    workspace: &str,
    relative: &str,
) -> Result<(), String> {
    use md5::Digest;
    use std::path::Component;
    if load_binding()?.as_ref().is_none_or(|current| {
        current.bot_id != binding.bot_id || current.user_id != binding.user_id
    }) {
        return Err("微信绑定已变化".into());
    }
    let relative = PathBuf::from(relative.trim());
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("请使用当前工作区内的相对文件路径".into());
    }
    let root = PathBuf::from(workspace)
        .canonicalize()
        .map_err(|_| "当前工作区不可访问")?;
    let candidate = root.join(&relative);
    let path = candidate.canonicalize().map_err(|_| "文件不存在")?;
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|_| "文件不存在")?;
    if !path.starts_with(&root) || !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("只能发送当前工作区内的普通文件".into());
    }
    if metadata.len() > crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES {
        return Err("文件不能超过 20 MB".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败：{e}"))?;
    if bytes.len() as u64 > crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES {
        return Err("文件不能超过 20 MB".into());
    }
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("文件名无效")?;
    let is_image = matches!(
        path.extension()
            .and_then(|v| v.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    ) && image::guess_format(&bytes).is_ok();
    let mut key = [0_u8; 16];
    let mut file_key = [0_u8; 16];
    getrandom::fill(&mut key).map_err(|_| "无法生成微信附件密钥")?;
    getrandom::fill(&mut file_key).map_err(|_| "无法生成微信附件标识")?;
    let encrypted = encrypt_media(bytes.clone(), &key)?;
    let digest = hex::encode(md5::Md5::digest(&bytes));
    let response = api_post(
        &binding.base_url,
        "ilink/bot/getuploadurl",
        Some(&token()?),
        json!({
            "filekey": hex::encode(file_key), "media_type": if is_image { 1 } else { 3 },
            "to_user_id": binding.user_id, "rawsize": bytes.len(), "rawfilemd5": digest,
            "filesize": encrypted.len(), "no_need_thumb": true, "aeskey": hex::encode(key),
            "base_info": base_info()
        }),
        Duration::from_secs(15),
    )
    .await?;
    let upload_url = if let Some(full) = response
        .get("upload_full_url")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        Url::parse(full).map_err(|_| "微信上传地址无效")?
    } else {
        let query = response
            .get("upload_param")
            .and_then(Value::as_str)
            .ok_or("微信未返回上传地址")?;
        Url::parse_with_params(
            "https://novac2c.cdn.weixin.qq.com/c2c/upload",
            &[
                ("encrypted_query_param", query),
                ("filekey", &hex::encode(file_key)),
            ],
        )
        .map_err(|_| "微信上传地址无效")?
    };
    let host = upload_url.host_str().ok_or("微信上传地址无效")?;
    if upload_url.scheme() != "https"
        || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
    {
        return Err("微信上传地址不在允许的域名内".into());
    }
    let upload = api_client(Duration::from_secs(60))?
        .post(upload_url)
        .header("Content-Type", "application/octet-stream")
        .body(encrypted.clone())
        .send()
        .await
        .map_err(|e| format!("上传微信附件失败：{e}"))?;
    if !upload.status().is_success() {
        return Err(format!("微信附件上传返回 HTTP {}", upload.status()));
    }
    let query = upload
        .headers()
        .get("x-encrypted-param")
        .and_then(|v| v.to_str().ok())
        .ok_or("微信附件上传缺少下载参数")?;
    let key64 = base64::engine::general_purpose::STANDARD.encode(hex::encode(key));
    let item = if is_image {
        json!({"type": 2, "image_item": {"media": {"encrypt_query_param": query, "aes_key": key64, "encrypt_type": 1}, "mid_size": encrypted.len()}})
    } else {
        json!({"type": 4, "file_item": {"media": {"encrypt_query_param": query, "aes_key": key64, "encrypt_type": 1}, "file_name": name, "len": bytes.len().to_string()}})
    };
    let _outbox_guard = OUTBOX_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let current = load_binding()?.ok_or("微信已解除绑定")?;
    if current.bot_id != binding.bot_id
        || current.user_id != binding.user_id
        || !accessible_sessions(app, &current)
            .iter()
            .any(|session| session.session_id == session_id)
    {
        return Err("此会话未授权微信访问".into());
    }
    api_post(
        &current.base_url,
        "ilink/bot/sendmessage",
        Some(&token()?),
        json!({
            "msg": {"from_user_id": "", "to_user_id": current.user_id,
                "client_id": Uuid::now_v7().to_string(), "message_type": 2,
                "message_state": 2, "context_token": current.context_token,
                "item_list": [item]},
            "base_info": base_info()
        }),
        Duration::from_secs(15),
    )
    .await?;
    Ok(())
}

async fn download_attachments(app: &AppHandle, message: &Value) -> Result<Vec<String>, String> {
    let Some(items) = message.get("item_list").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if items
        .iter()
        .filter(|item| matches!(item.get("type").and_then(Value::as_u64), Some(2 | 4)))
        .count()
        > 10
    {
        return Err("单条消息附件不能超过 10 个".into());
    }
    let mut saved = Vec::new();
    let mut total_bytes = 0_u64;
    for item in items {
        let kind = item.get("type").and_then(Value::as_u64).unwrap_or(0);
        let (details, name, mime) = match kind {
            2 => (
                item.get("image_item"),
                Some("image.jpg".to_string()),
                "image/jpeg",
            ),
            4 => (
                item.get("file_item"),
                item.get("file_item")
                    .and_then(|v| v.get("file_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                "application/octet-stream",
            ),
            _ => continue,
        };
        let details = details.ok_or("微信附件信息不完整")?;
        let media = details.get("media").ok_or("微信附件缺少媒体引用")?;
        let url = cdn_url(media)?;
        let mut response = api_client(Duration::from_secs(60))?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("下载微信附件失败：{e}"))?;
        if !response.status().is_success() {
            return Err(format!("下载微信附件返回 HTTP {}", response.status()));
        }
        let max = crate::shell_fs::MAX_ATTACHMENT_FILE_BYTES as usize;
        if response
            .content_length()
            .is_some_and(|length| length as usize > max + 16)
        {
            return Err("微信附件超过大小限制".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("读取微信附件失败：{e}"))?
        {
            if bytes.len() + chunk.len() > max + 16 {
                return Err("微信附件超过大小限制".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let aes_key = details
            .get("aeskey")
            .and_then(Value::as_str)
            .map(|key| (key, true))
            .or_else(|| {
                media
                    .get("aes_key")
                    .and_then(Value::as_str)
                    .map(|key| (key, false))
            });
        if let Some((key, hex_key)) = aes_key {
            bytes = decrypt_media(bytes, key, hex_key)?;
        }
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or("微信附件总大小超出限制")?;
        if total_bytes > crate::shell_fs::MAX_ATTACHMENT_TOTAL_BYTES {
            return Err("单条消息的附件总大小不能超过 64 MB".into());
        }
        let (name, mime) = if kind == 2 {
            match image::guess_format(&bytes).map_err(|_| "微信图片格式不受支持")? {
                image::ImageFormat::Png => ("image.png".to_string(), "image/png"),
                image::ImageFormat::Jpeg => ("image.jpg".to_string(), "image/jpeg"),
                image::ImageFormat::Gif => ("image.gif".to_string(), "image/gif"),
                image::ImageFormat::WebP => ("image.webp".to_string(), "image/webp"),
                _ => return Err("微信图片格式不受支持".into()),
            }
        } else {
            (name.ok_or("微信文件缺少文件名")?, mime)
        };
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("解析附件目录失败：{e}"))?
            .join("clipboard-images");
        let path = crate::attachment_blob::save_blob(&dir, &bytes, mime, Some(&name))?;
        app.state::<crate::shell_fs::FilesystemAccess>()
            .require_managed_attachment(&path)?;
        if let Err(error) = crate::attachment_blob::prune_store(&dir, Some(&path)) {
            tracing::warn!(%error, "failed to prune Weixin attachments");
        }
        saved.push(path.to_string_lossy().into_owned());
    }
    Ok(saved)
}

fn token() -> Result<String, String> {
    crate::org::credential_read(CREDENTIAL_ACCOUNT)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信绑定凭据不存在，请重新扫码".into())
}

fn default_workspace(binding: &Binding) -> Option<&str> {
    binding
        .default_workspace
        .as_deref()
        .filter(|cwd| {
            binding
                .allowed_workspaces
                .iter()
                .any(|allowed| allowed == cwd)
        })
        .or_else(|| binding.allowed_workspaces.first().map(String::as_str))
}

#[tauri::command]
pub(crate) fn weixin_status(state: State<'_, WeixinState>) -> Result<WeixinStatus, String> {
    let binding = load_binding()?;
    let connected = binding.is_some()
        && crate::org::credential_read(CREDENTIAL_ACCOUNT)?.is_some_and(|value| !value.is_empty());
    let sessions = crate::sessions::list_all_sessions(true).unwrap_or_default();
    let active_session_title = binding
        .as_ref()
        .and_then(|value| value.active_session.as_ref())
        .and_then(|id| sessions.iter().find(|session| &session.session_id == id))
        .map(|session| session.title.clone());
    let shared_sessions = binding
        .as_ref()
        .map(|value| {
            value
                .shared_sessions
                .iter()
                .map(|id| {
                    let session = sessions.iter().find(|session| &session.session_id == id);
                    SharedSession {
                        session_id: id.clone(),
                        title: session
                            .map(|session| session.title.clone())
                            .unwrap_or_else(|| "已移除的任务".into()),
                        cwd: session
                            .map(|session| session.cwd.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(WeixinStatus {
        connected,
        bot_id: binding.as_ref().map(|value| value.bot_id.clone()),
        active_session: binding
            .as_ref()
            .and_then(|value| value.active_session.clone()),
        active_session_title,
        allowed_workspaces: binding
            .as_ref()
            .map(|value| value.allowed_workspaces.clone())
            .unwrap_or_default(),
        default_workspace: binding
            .as_ref()
            .and_then(default_workspace)
            .map(str::to_string),
        shared_sessions,
        pending_replies: binding.as_ref().map_or(0, |value| value.outbox.len()),
        online: connected
            && state.worker_alive.load(Ordering::SeqCst)
            && state.outbox_alive.load(Ordering::SeqCst)
            && state.last_error.lock().unwrap().is_none()
            && state
                .paused_until
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|until| Instant::now() >= *until)
            && state
                .last_success
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|instant| {
                    instant
                        .elapsed()
                        .is_ok_and(|age| age < Duration::from_secs(90))
                }),
        last_error: state
            .outbox_error
            .lock()
            .unwrap()
            .clone()
            .or_else(|| state.last_error.lock().unwrap().clone()),
    })
}

#[tauri::command]
pub(crate) async fn weixin_qr_start(state: State<'_, WeixinState>) -> Result<QrStart, String> {
    let generation = {
        let mut login = state.login.lock().unwrap();
        *login = None;
        state.login_generation.fetch_add(1, Ordering::SeqCst) + 1
    };
    let local_tokens: Vec<String> = if load_binding()?.is_some() {
        crate::org::credential_read(CREDENTIAL_ACCOUNT)?
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let result = api_post(
        API_BASE,
        "ilink/bot/get_bot_qrcode?bot_type=3",
        None,
        json!({"local_token_list": local_tokens}),
        Duration::from_secs(15),
    )
    .await?;
    let qrcode = result
        .get("qrcode")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("微信未返回二维码")?;
    let qr_url = result
        .get("qrcode_img_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("微信未返回二维码链接")?;
    let mut login = state.login.lock().unwrap();
    if state.login_generation.load(Ordering::SeqCst) != generation {
        return Err("二维码已刷新，请使用最新的二维码".into());
    }
    *login = Some(Login {
        qrcode: qrcode.into(),
        base_url: API_BASE.into(),
        created: Instant::now(),
    });
    Ok(QrStart {
        qr_url: qr_url.into(),
    })
}

#[tauri::command]
pub(crate) async fn weixin_qr_poll(
    app: AppHandle,
    state: State<'_, WeixinState>,
    verify_code: Option<String>,
) -> Result<QrPoll, String> {
    let login = state
        .login
        .lock()
        .unwrap()
        .clone()
        .ok_or("请先获取绑定二维码")?;
    if login.created.elapsed() > Duration::from_secs(300) {
        let mut active = state.login.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|value| value.qrcode == login.qrcode)
        {
            *active = None;
        }
        return Ok(QrPoll {
            status: "expired".into(),
            connected: false,
        });
    }
    let mut path = format!(
        "ilink/bot/get_qrcode_status?qrcode={}",
        urlencoding::encode(&login.qrcode)
    );
    if let Some(code) = verify_code.as_deref() {
        if code.len() > 32 || !code.chars().all(|ch| ch.is_ascii_digit()) {
            return Err("配对码必须为数字".into());
        }
        path.push_str("&verify_code=");
        path.push_str(code);
    }
    let result = match api_get(&login.base_url, &path, Duration::from_secs(40)).await {
        Ok(value) => value,
        Err(error) if error.contains("超时") => {
            return Ok(QrPoll {
                status: "wait".into(),
                connected: false,
            })
        }
        Err(error) => return Err(error),
    };
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("wait");
    if status == "scaned_but_redirect" {
        if let Some(host) = result.get("redirect_host").and_then(Value::as_str) {
            let redirected = format!("https://{host}");
            validate_base_url(&redirected)?;
            if let Some(active) = state.login.lock().unwrap().as_mut() {
                if active.qrcode == login.qrcode {
                    active.base_url = redirected;
                }
            }
        }
    }
    if status == "binded_redirect" {
        let connected = load_binding()?.is_some() && token().is_ok();
        if connected {
            *state.login.lock().unwrap() = None;
            start_worker(&app);
        }
        return Ok(QrPoll {
            status: status.into(),
            connected,
        });
    }
    if status == "confirmed" {
        let mut active = state.login.lock().unwrap();
        if active
            .as_ref()
            .is_none_or(|value| value.qrcode != login.qrcode)
        {
            return Err("二维码已刷新，请使用最新的二维码".into());
        }
        let bot_id = result
            .get("ilink_bot_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("微信绑定缺少机器人标识")?;
        let user_id = result
            .get("ilink_user_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("微信绑定缺少用户标识")?;
        let bot_token = result
            .get("bot_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("微信绑定缺少凭据")?;
        let base_url = result
            .get("baseurl")
            .and_then(Value::as_str)
            .unwrap_or(API_BASE);
        validate_base_url(base_url)?;
        crate::org::credential_write(CREDENTIAL_ACCOUNT, bot_token)?;
        let save_result = (|| -> Result<(), String> {
            let _guard = BINDING_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
            let previous = load_binding()?.filter(|value| value.user_id == user_id);
            let mut binding = previous.unwrap_or_default();
            binding.bot_id = bot_id.into();
            binding.user_id = user_id.into();
            binding.base_url = base_url.into();
            binding.cursor.clear();
            binding.context_token = None;
            binding.seen_ids.clear();
            binding.inflight.clear();
            binding.outbound_routes.clear();
            binding.outbound_pending.clear();
            binding.outbox.clear();
            save_binding(&binding)
        })();
        if let Err(error) = save_result {
            let _ = crate::org::credential_delete(CREDENTIAL_ACCOUNT);
            return Err(error);
        }
        *active = None;
        drop(active);
        start_worker(&app);
    }
    Ok(QrPoll {
        status: status.into(),
        connected: status == "confirmed",
    })
}

#[tauri::command]
pub(crate) async fn weixin_set_workspaces(
    app: AppHandle,
    workspaces: Vec<String>,
    default_workspace: Option<String>,
) -> Result<WeixinStatus, String> {
    let _outbox_guard = OUTBOX_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if workspaces.len() > 24 {
        return Err("最多允许 24 个远程工作区".into());
    }
    let access = app.state::<crate::shell_fs::FilesystemAccess>();
    let mut allowed = Vec::new();
    for cwd in workspaces {
        let canonical = access
            .require_workspace(&cwd)?
            .to_string_lossy()
            .into_owned();
        if !allowed.contains(&canonical) {
            allowed.push(canonical);
        }
    }
    let preferred = default_workspace
        .map(|cwd| {
            access
                .require_workspace(&cwd)
                .map(|path| path.to_string_lossy().into_owned())
        })
        .transpose()?;
    if preferred.as_ref().is_some_and(|cwd| !allowed.contains(cwd)) {
        return Err("默认工作区必须已授权微信访问".into());
    }
    let sessions = crate::sessions::list_all_sessions(false)?;
    mutate_binding(|binding| {
        binding.allowed_workspaces = allowed;
        binding.default_workspace =
            preferred.or_else(|| self::default_workspace(binding).map(str::to_string));
        if binding.active_session.as_ref().is_some_and(|id| {
            !sessions.iter().any(|session| {
                &session.session_id == id && session_accessible(binding, session, &access)
            })
        }) {
            binding.active_session = None;
        }
        Ok(())
    })?;
    weixin_status(app.state::<WeixinState>())
}

#[tauri::command]
pub(crate) async fn weixin_revoke_session(
    app: AppHandle,
    session_id: String,
) -> Result<WeixinStatus, String> {
    let _outbox_guard = OUTBOX_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let session = crate::sessions::list_all_sessions(false)?
        .into_iter()
        .find(|session| session.session_id == session_id);
    let access = app.state::<crate::shell_fs::FilesystemAccess>();
    let mut retained_access = false;
    mutate_binding(|binding| {
        retained_access = session.as_ref().is_some_and(|session| {
            !session.hidden
                && access.require_workspace(&session.cwd).is_ok_and(|cwd| {
                    binding
                        .allowed_workspaces
                        .iter()
                        .any(|root| root == &cwd.to_string_lossy())
                })
        });
        revoke_session_access(binding, &session_id, retained_access)
    })?;
    if !retained_access {
        app.state::<WeixinState>()
            .pending
            .lock()
            .unwrap()
            .retain(|_, pending| pending.session_id != session_id);
        app.state::<WeixinState>()
            .progress_at
            .lock()
            .unwrap()
            .remove(&session_id);
    }
    weixin_status(app.state::<WeixinState>())
}

fn revoke_session_access(
    binding: &mut Binding,
    session_id: &str,
    retained_access: bool,
) -> Result<(), String> {
    if !binding.shared_sessions.iter().any(|id| id == session_id) {
        return Err("任务未单独授权微信访问".into());
    }
    binding.shared_sessions.retain(|id| id != session_id);
    if !retained_access {
        binding
            .outbox
            .retain(|message| message.session_id.as_deref() != Some(session_id));
        binding
            .outbound_pending
            .retain(|(_, target)| target != session_id);
        if binding.active_session.as_deref() == Some(session_id) {
            binding.active_session = None;
        }
    }
    Ok(())
}

fn session_accessible(
    binding: &Binding,
    session: &SessionSummary,
    access: &crate::shell_fs::FilesystemAccess,
) -> bool {
    if session.hidden {
        return false;
    }
    let Ok(cwd) = access.require_workspace(&session.cwd) else {
        return false;
    };
    binding
        .allowed_workspaces
        .iter()
        .any(|root| root == &cwd.to_string_lossy())
        || binding
            .shared_sessions
            .iter()
            .any(|id| id == &session.session_id)
}

fn accessible_sessions(app: &AppHandle, binding: &Binding) -> Vec<SessionSummary> {
    let access = app.state::<crate::shell_fs::FilesystemAccess>();
    crate::sessions::list_all_sessions(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|session| session_accessible(binding, session, &access))
        .collect()
}

fn short_id(id: &str) -> &str {
    id.get(id.len().saturating_sub(8)..).unwrap_or(id)
}

fn task_page_command(input: &str) -> Result<Option<usize>, String> {
    let input = input.trim();
    let rest = input
        .strip_prefix("任务")
        .or_else(|| input.strip_prefix("/sessions"));
    let Some(rest) = rest else {
        return Ok(None);
    };
    if rest.is_empty() {
        return Ok(Some(1));
    }
    if !rest
        .chars()
        .next()
        .is_some_and(|first| first.is_whitespace() || first.is_ascii_digit())
    {
        return Ok(None);
    }
    let number = rest.trim();
    if number.is_empty() {
        return Ok(Some(1));
    }
    let page = number
        .parse::<usize>()
        .map_err(|_| "页码无效，请发送“任务2”或“任务 2”翻页".to_string())?;
    if !(1..10_000).contains(&page) {
        return Err("页码无效，请发送“任务2”或“任务 2”翻页".into());
    }
    Ok(Some(page))
}

fn weixin_list_label(value: &str, max_chars: usize) -> String {
    let clean: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let normalized = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut label: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

fn format_task_page(
    sessions: &[SessionSummary],
    active_session: Option<&str>,
    page: usize,
) -> Result<String, String> {
    let total_pages = sessions.len().div_ceil(TASKS_PER_PAGE).max(1);
    if page == 0 || page > total_pages {
        return Err(format!(
            "只有 {total_pages} 页任务，请发送“任务”从第一页查看"
        ));
    }
    if sessions.is_empty() {
        return Ok(
            "尚无可通过微信访问的会话。请在桌面任务中选择“在微信继续”，或前往“设置 → 消息通道”授权工作区。"
                .into(),
        );
    }

    let mut sections = vec![format!(
        "可继续的任务 · {page}/{total_pages}（共 {} 个）",
        sessions.len()
    )];
    for session in sessions
        .iter()
        .skip((page - 1) * TASKS_PER_PAGE)
        .take(TASKS_PER_PAGE)
    {
        let title = weixin_list_label(&session.title, 32);
        let title = if title.is_empty() {
            "未命名任务"
        } else {
            &title
        };
        let folder = PathBuf::from(&session.cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| session.cwd.clone());
        let folder = weixin_list_label(&folder, 18);
        let folder = if folder.is_empty() {
            "工作区"
        } else {
            &folder
        };
        let current = if active_session == Some(session.session_id.as_str()) {
            "【当前】"
        } else {
            ""
        };
        sections.push(format!(
            "• {current}{title}\n  #{} · {folder}",
            short_id(&session.session_id)
        ));
    }
    let mut guidance = String::from("切换任务：发送“切换 #编号”");
    if page > 1 {
        guidance.push_str(&format!("\n上一页：发送“任务{}”", page - 1));
    }
    if page < total_pages {
        guidance.push_str(&format!("\n下一页：发送“任务{}”", page + 1));
    }
    sections.push(guidance);
    Ok(sections.join("\n\n"))
}

fn reviewable_permission_details(raw_input: Option<&Value>) -> Option<String> {
    let input = raw_input?;
    if input.is_null() || input.get("_truncated").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let details = input.to_string();
    (details.chars().count() <= 2400).then_some(details)
}

#[tauri::command]
pub(crate) async fn weixin_handoff(app: AppHandle, session_id: String) -> Result<String, String> {
    let binding = load_binding()?.ok_or("请先前往“设置 → 消息通道”绑定微信")?;
    token()?;
    let session = crate::sessions::list_all_sessions(true)?
        .into_iter()
        .find(|entry| {
            entry.session_id == session_id && !entry.hidden && entry.archived != Some(true)
        })
        .ok_or("会话不存在")?;
    app.state::<crate::shell_fs::FilesystemAccess>()
        .require_workspace(&session.cwd)?;
    let binding = mutate_binding(|current| {
        if current.bot_id != binding.bot_id {
            return Err("微信绑定已变化，请重试".into());
        }
        if !current.shared_sessions.contains(&session_id) {
            if current.shared_sessions.len() >= 128 {
                return Err(
                    "单独交接任务已达 128 个，请在“设置 → 消息通道”撤销不再需要的任务".into(),
                );
            }
            current.shared_sessions.push(session_id.clone());
        }
        current.active_session = Some(session_id.clone());
        Ok(())
    })?;
    if matches!(session.status.as_deref(), Some("working" | "planning")) {
        app.state::<WeixinState>()
            .progress_at
            .lock()
            .unwrap()
            .insert(session_id.clone(), Instant::now());
    }
    let text = format!(
        "已切换至 #{}《{}》\n项目：{}\n在微信直接发送消息即可继续。",
        short_id(&session.session_id),
        session.title,
        session.cwd
    );
    if binding.context_token.is_some() {
        return match send_text(&binding, &text, Some(&session.session_id)).await {
            Ok(()) => Ok("会话已交接，微信通知已加入发送队列".into()),
            Err(error) => Ok(format!(
                "会话已交接；微信通知未能排队：{error}。可在微信发送“状态”查看。"
            )),
        };
    }
    Ok("已绑定会话；请先在微信向 Bot 发送“状态”以建立聊天".into())
}

async fn notify_lifecycle(binding: &Binding, credential: &str, start: bool) -> Result<(), String> {
    api_post(
        &binding.base_url,
        if start {
            "ilink/bot/msg/notifystart"
        } else {
            "ilink/bot/msg/notifystop"
        },
        Some(credential),
        json!({ "base_info": base_info() }),
        Duration::from_secs(8),
    )
    .await
    .map(|_| ())
}

fn shutdown_identity(
    binding: Result<Option<Binding>, String>,
    read_credential: impl FnOnce() -> Result<String, String>,
) -> Option<(Binding, String)> {
    // An unbound profile has no lifecycle notification to send. In particular,
    // never open a system credential prompt just because the app is quitting.
    let binding = binding.ok().flatten()?;
    Some((binding, read_credential().ok()?))
}

pub(crate) async fn shutdown(app: &AppHandle) {
    let state = app.state::<WeixinState>();
    state.worker_generation.fetch_add(1, Ordering::SeqCst);
    state.worker_alive.store(false, Ordering::SeqCst);
    state.outbox_alive.store(false, Ordering::SeqCst);
    if let Some(worker) = state.worker.lock().unwrap().take() {
        worker.abort();
    }
    if let Some(worker) = state.outbox_worker.lock().unwrap().take() {
        worker.abort();
    }
    if let Some((binding, credential)) = shutdown_identity(load_binding(), token) {
        if let Err(error) = notify_lifecycle(&binding, &credential, false).await {
            tracing::warn!(%error, "failed to notify Weixin channel stop");
        }
    }
}

#[tauri::command]
pub(crate) async fn weixin_disconnect(app: AppHandle) -> Result<(), String> {
    let state = app.state::<WeixinState>();
    {
        let mut login = state.login.lock().unwrap();
        *login = None;
        state.login_generation.fetch_add(1, Ordering::SeqCst);
    }
    shutdown(&app).await;
    *app.state::<WeixinState>().last_success.lock().unwrap() = None;
    *app.state::<WeixinState>().last_error.lock().unwrap() = None;
    *app.state::<WeixinState>().outbox_error.lock().unwrap() = None;
    *app.state::<WeixinState>().paused_until.lock().unwrap() = None;
    app.state::<WeixinState>()
        .progress_at
        .lock()
        .unwrap()
        .clear();
    app.state::<WeixinState>().pending.lock().unwrap().clear();
    app.state::<WeixinState>().output.lock().unwrap().clear();
    let _guard = BINDING_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    crate::org::credential_delete(CREDENTIAL_ACCOUNT)?;
    match std::fs::remove_file(state_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除微信通道配置失败：{error}")),
    }
}

pub(crate) fn init(app: &AppHandle) {
    let update_app = app.clone();
    app.listen("agent://update", move |event| {
        let Ok(value) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let kind = value
            .get("sessionUpdate")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str);
        if kind == Some("plan_approval_resolved") {
            if let Some(id) = value.get("requestId").and_then(Value::as_str) {
                update_app
                    .state::<WeixinState>()
                    .pending
                    .lock()
                    .unwrap()
                    .remove(&short_id(id).to_uppercase());
            }
            return;
        }
        if matches!(
            kind,
            Some("agent_message_chunk" | "tool_call" | "tool_call_update")
        ) {
            let state = update_app.state::<WeixinState>();
            let should_notify = {
                let mut progress = state.progress_at.lock().unwrap();
                if let Some(last) = progress.get_mut(session_id) {
                    if last.elapsed() >= Duration::from_secs(90) {
                        *last = Instant::now();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if should_notify {
                if let Some(binding) = load_binding().ok().flatten().filter(|binding| {
                    accessible_sessions(&update_app, binding)
                        .iter()
                        .any(|session| session.session_id == session_id)
                }) {
                    let text = format!(
                        "#{} 任务仍在运行，可随时发送“状态”查看当前会话。",
                        short_id(session_id)
                    );
                    let session_id = session_id.to_string();
                    tauri::async_runtime::spawn(async move {
                        let _ = send_text(&binding, &text, Some(&session_id)).await;
                    });
                }
            }
        }
        if kind != Some("agent_message_chunk") {
            return;
        }
        let Some(content) = value.get("content") else {
            return;
        };
        let delta = if let Some(blocks) = content.as_array() {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>()
        } else {
            content
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        if delta.is_empty() {
            return;
        }
        let state = update_app.state::<WeixinState>();
        let mut text = state.output.lock().unwrap();
        let output = text.entry(session_id.to_string()).or_default();
        if output.len() < 32_000 {
            output.push_str(&delta);
        }
    });
    let complete_app = app.clone();
    app.listen("agent://complete", move |event| {
        let Ok(complete) = serde_json::from_str::<CompleteEvent>(event.payload()) else {
            return;
        };
        let output = complete_app
            .state::<WeixinState>()
            .output
            .lock()
            .unwrap()
            .remove(&complete.session_id);
        complete_app
            .state::<WeixinState>()
            .progress_at
            .lock()
            .unwrap()
            .remove(&complete.session_id);
        let Some(binding) = load_binding().ok().flatten() else {
            return;
        };
        let Some(session) = accessible_sessions(&complete_app, &binding)
            .into_iter()
            .find(|s| s.session_id == complete.session_id)
        else {
            return;
        };
        if binding.active_session.as_deref() != Some(&complete.session_id)
            && !binding.shared_sessions.contains(&complete.session_id)
        {
            return;
        }
        let mut body = if complete.stop_reason == "end_turn" {
            output
                .filter(|text| !text.trim().is_empty())
                .or(complete.agent_result.filter(|text| !text.trim().is_empty()))
                .unwrap_or_else(|| "任务已完成，可在桌面查看完整记录。".into())
        } else {
            format!("任务结束：{}", complete.stop_reason)
        };
        if body.len() >= 32_000 {
            body.push_str("\n（回复较长，完整记录请在桌面查看。）");
        }
        let text = format!(
            "#{}《{}》\n{}",
            short_id(&session.session_id),
            session.title,
            body
        );
        tauri::async_runtime::spawn(async move {
            if let Err(error) = send_text(&binding, &text, Some(&session.session_id)).await {
                tracing::warn!(%error, "failed to deliver Weixin task result");
            }
        });
    });
    let permission_app = app.clone();
    app.listen("agent://permission", move |event| {
        let Ok(permission) = serde_json::from_str::<PermissionFrontend>(event.payload()) else {
            return;
        };
        let Some(binding) = load_binding().ok().flatten() else {
            return;
        };
        if !accessible_sessions(&permission_app, &binding)
            .iter()
            .any(|s| s.session_id == permission.session_id)
        {
            return;
        }
        let allow = permission
            .options
            .iter()
            .find(|option| option.kind == "allow");
        let deny = permission
            .options
            .iter()
            .find(|option| option.kind == "deny");
        if allow.is_none() && deny.is_none() {
            return;
        }
        let code = short_id(&permission.request_id).to_uppercase();
        let details = reviewable_permission_details(permission.raw_input.as_ref());
        let can_allow = allow.is_some()
            && details.as_ref().is_some_and(|details| {
                details.chars().count()
                    + permission.title.chars().count()
                    + permission.tool_kind.chars().count()
                    + 120
                    <= MAX_REPLY_CHARS - 80
            });
        permission_app
            .state::<WeixinState>()
            .pending
            .lock()
            .unwrap()
            .insert(
                code.clone(),
                Pending {
                    request_id: permission.request_id,
                    session_id: permission.session_id.clone(),
                    kind: PendingKind::Permission {
                        allow: can_allow
                            .then(|| allow.map(|v| v.option_id.clone()))
                            .flatten(),
                        deny: deny.map(|v| v.option_id.clone()),
                    },
                },
            );
        let text = if can_allow {
            let details = details.expect("can_allow requires complete parameters");
            format!(
                "#{} 需要确认：{}\n工具：{}\n操作参数：{}\n回复“允许 {}”{}。",
                short_id(&permission.session_id),
                permission.title,
                permission.tool_kind,
                details,
                code,
                if deny.is_some() {
                    format!("或“拒绝 {}”", code)
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "#{} 操作参数无法在微信完整核实，请在桌面审阅后批准{}。",
                short_id(&permission.session_id),
                if deny.is_some() {
                    format!("；微信可回复“拒绝 {}”", code)
                } else {
                    String::new()
                }
            )
        };
        tauri::async_runtime::spawn(async move {
            let _ = send_interaction_text(&binding, &text, &permission.session_id, &code).await;
        });
    });
    let question_app = app.clone();
    app.listen("agent://question", move |event| {
        let Ok(question) = serde_json::from_str::<QuestionFrontend>(event.payload()) else {
            return;
        };
        let Some(binding) = load_binding().ok().flatten() else {
            return;
        };
        if !accessible_sessions(&question_app, &binding)
            .iter()
            .any(|s| s.session_id == question.session_id)
        {
            return;
        }
        let prompts: Vec<_> = question
            .questions
            .iter()
            .map(|q| q.question.clone())
            .collect();
        if prompts.is_empty() {
            return;
        }
        let code = short_id(&question.request_id).to_uppercase();
        question_app
            .state::<WeixinState>()
            .pending
            .lock()
            .unwrap()
            .insert(
                code.clone(),
                Pending {
                    request_id: question.request_id,
                    session_id: question.session_id.clone(),
                    kind: PendingKind::Question {
                        questions: prompts.clone(),
                    },
                },
            );
        let list = prompts
            .iter()
            .enumerate()
            .map(|(i, text)| format!("{}. {}", i + 1, text))
            .collect::<Vec<_>>()
            .join("\n");
        let answer_hint = if prompts.len() == 1 {
            format!("回答 {code} 你的答案")
        } else {
            format!("回答 {code} 1=答案;2=答案")
        };
        let text = format!(
            "#{} 需要回答：\n{}\n回复“{}”。",
            short_id(&question.session_id),
            list,
            answer_hint
        );
        tauri::async_runtime::spawn(async move {
            let _ = send_interaction_text(&binding, &text, &question.session_id, &code).await;
        });
    });
    let plan_app = app.clone();
    app.listen("agent://plan-approval", move |event| {
        let Ok(plan) = serde_json::from_str::<crate::bridge::PlanApprovalFrontend>(event.payload())
        else {
            return;
        };
        let Some(binding) = load_binding().ok().flatten() else {
            return;
        };
        if !accessible_sessions(&plan_app, &binding)
            .iter()
            .any(|s| s.session_id == plan.session_id)
        {
            return;
        }
        let code = short_id(&plan.request_id).to_uppercase();
        let detail = plan
            .plan_content
            .unwrap_or_else(|| "请在桌面查看方案详情".into());
        let can_approve = detail.chars().count() <= 2400 && detail != "请在桌面查看方案详情";
        plan_app
            .state::<WeixinState>()
            .pending
            .lock()
            .unwrap()
            .insert(
                code.clone(),
                Pending {
                    request_id: plan.request_id,
                    session_id: plan.session_id.clone(),
                    kind: PendingKind::Plan { can_approve },
                },
            );
        let text = if can_approve {
            format!(
                "#{} 方案待确认：\n{}\n回复“通过 {}”或“拒绝 {}”。",
                short_id(&plan.session_id),
                detail,
                code,
                code
            )
        } else {
            format!(
                "#{} 方案较长，请在桌面审阅后批准；微信可回复“拒绝 {}”。",
                short_id(&plan.session_id),
                code
            )
        };
        tauri::async_runtime::spawn(async move {
            let _ = send_interaction_text(&binding, &text, &plan.session_id, &code).await;
        });
    });
    let closed_app = app.clone();
    for event_name in ["agent://permission-closed", "agent://question-closed"] {
        let closed_app = closed_app.clone();
        app.listen(event_name, move |event| {
            let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                return;
            };
            let Some(id) = payload.get("requestId").and_then(Value::as_str) else {
                return;
            };
            closed_app
                .state::<WeixinState>()
                .pending
                .lock()
                .unwrap()
                .remove(&short_id(id).to_uppercase());
        });
    }
    start_worker(app);
}

fn start_worker(app: &AppHandle) {
    let state = app.state::<WeixinState>();
    let generation = state.worker_generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.worker_alive.store(false, Ordering::SeqCst);
    state.outbox_alive.store(false, Ordering::SeqCst);
    if let Some(old) = state.worker.lock().unwrap().take() {
        old.abort();
    }
    if let Some(old) = state.outbox_worker.lock().unwrap().take() {
        old.abort();
    }
    *state.last_success.lock().unwrap() = None;
    *state.last_error.lock().unwrap() = None;
    *state.outbox_error.lock().unwrap() = None;
    *state.paused_until.lock().unwrap() = None;
    if matches!(load_binding(), Ok(None)) {
        return;
    }
    let app = app.clone();
    let outbox_app = app.clone();
    *state.worker.lock().unwrap() = Some(tauri::async_runtime::spawn(async move {
        let _guard = WorkerGuard {
            app: app.clone(),
            generation,
            outbox: false,
        };
        app.state::<WeixinState>()
            .worker_alive
            .store(true, Ordering::SeqCst);
        worker_loop(app).await;
    }));
    *state.outbox_worker.lock().unwrap() = Some(tauri::async_runtime::spawn(async move {
        let _guard = WorkerGuard {
            app: outbox_app.clone(),
            generation,
            outbox: true,
        };
        outbox_app
            .state::<WeixinState>()
            .outbox_alive
            .store(true, Ordering::SeqCst);
        outbox_loop(outbox_app).await;
    }));
}

async fn outbox_loop(app: AppHandle) {
    let mut failures = 0_u32;
    loop {
        let notified = OUTBOX_NOTIFY
            .get_or_init(tokio::sync::Notify::new)
            .notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let binding = match load_binding() {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(error) => {
                *app.state::<WeixinState>().outbox_error.lock().unwrap() =
                    Some(format!("读取微信发送队列失败，正在重试：{error}"));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let pause = app
            .state::<WeixinState>()
            .paused_until
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|until| until.checked_duration_since(Instant::now()));
        if let Some(remaining) = pause {
            tokio::time::sleep(remaining.min(Duration::from_secs(30))).await;
            continue;
        }
        if binding.outbox.is_empty() {
            *app.state::<WeixinState>().outbox_error.lock().unwrap() = None;
            notified.await;
            continue;
        }
        if let Err(error) = token() {
            *app.state::<WeixinState>().outbox_error.lock().unwrap() =
                Some(format!("读取微信凭据失败，正在重试：{error}"));
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        match deliver_outbox_head(&app).await {
            Ok(()) => {
                failures = 0;
                *app.state::<WeixinState>().outbox_error.lock().unwrap() = None;
            }
            Err(error) => {
                tracing::warn!(%error, "Weixin reply remains queued");
                let paused = error.contains("错误 -14");
                if paused {
                    *app.state::<WeixinState>().paused_until.lock().unwrap() =
                        Some(Instant::now() + Duration::from_secs(3600));
                }
                *app.state::<WeixinState>().outbox_error.lock().unwrap() = Some(if paused {
                    "微信服务要求暂停连接 1 小时，待发送回复将在恢复后重试".into()
                } else {
                    format!("微信回复发送失败，正在重试：{error}")
                });
                failures = (failures + 1).min(5);
                tokio::time::sleep(Duration::from_secs((5_u64 << (failures - 1)).min(80))).await;
            }
        }
    }
}

async fn deliver_outbox_head(app: &AppHandle) -> Result<(), String> {
    let _guard = OUTBOX_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let binding = load_binding()?.ok_or("微信已解除绑定")?;
    let Some(message) = binding.outbox.front().cloned() else {
        return Ok(());
    };
    let still_authorized = if let Some(id) = &message.session_id {
        let sessions = crate::sessions::list_all_sessions(false)?;
        let access = app.state::<crate::shell_fs::FilesystemAccess>();
        sessions.iter().any(|session| {
            &session.session_id == id && session_accessible(&binding, session, &access)
        })
    } else {
        true
    };
    let still_pending = message.request_code.as_ref().is_none_or(|code| {
        app.state::<WeixinState>()
            .pending
            .lock()
            .unwrap()
            .contains_key(code)
    });
    if !still_authorized || !still_pending {
        mutate_binding(|current| {
            if current.bot_id == binding.bot_id
                && current
                    .outbox
                    .front()
                    .is_some_and(|front| front.client_id == message.client_id)
            {
                current.outbox.pop_front();
            }
            Ok(())
        })?;
        return Ok(());
    }
    if let Some(session_id) = &message.session_id {
        mutate_binding(|current| {
            if current.bot_id == binding.bot_id
                && !current
                    .outbound_pending
                    .iter()
                    .any(|(id, _)| id == &message.client_id)
            {
                current
                    .outbound_pending
                    .push_back((message.client_id.clone(), session_id.clone()));
                while current.outbound_pending.len() > MAX_ROUTES {
                    current.outbound_pending.pop_front();
                }
            }
            Ok(())
        })?;
    }
    let response = api_post(
        &binding.base_url,
        "ilink/bot/sendmessage",
        Some(&token()?),
        json!({
            "msg": {
                "from_user_id": "", "to_user_id": binding.user_id,
                "client_id": message.client_id, "message_type": 2,
                "message_state": 2,
                "context_token": reply_context(&message, &binding),
                "item_list": [{"type": 1, "text_item": {"text": message.text}}]
            },
            "base_info": base_info()
        }),
        Duration::from_secs(15),
    )
    .await?;
    let response_id = response.get("message_id").and_then(|value| {
        value
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .or_else(|| value.as_u64().map(|number| number.to_string()))
    });
    mutate_binding(|current| {
        if current.bot_id != binding.bot_id || current.user_id != binding.user_id {
            return Ok(());
        }
        if current
            .outbox
            .front()
            .is_some_and(|front| front.client_id == message.client_id)
        {
            current.outbox.pop_front();
            if let Some(session_id) = &message.session_id {
                if let Some(message_id) = response_id {
                    remember_outbound_route(current, message_id, session_id.clone());
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn recover_inflight(binding: &Binding) -> Result<(), String> {
    loop {
        let current = load_binding()?.ok_or("微信已解除绑定")?;
        if current.bot_id != binding.bot_id || current.user_id != binding.user_id {
            return Err("微信绑定已变化".into());
        }
        let Some(attempt) = current.inflight.front().cloned() else {
            return Ok(());
        };
        mutate_binding(|current| {
            if current.bot_id != binding.bot_id || current.user_id != binding.user_id {
                return Err("微信绑定已变化".into());
            }
            if current
                .inflight
                .front()
                .is_none_or(|front| front.message_id != attempt.message_id)
            {
                return Ok(());
            }
            if current.outbox.len() >= MAX_OUTBOX {
                return Err("微信待发送消息过多，暂缓接收新指令".into());
            }
            current.outbox.push_back(OutboundText {
                client_id: Uuid::now_v7().to_string(),
                text: format!(
                    "上次微信指令的执行结果未能确认：{}。请先发送“状态”或在桌面核对，再决定是否重发。",
                    attempt.description
                ),
                session_id: None,
                context_token: current.context_token.clone(),
                request_code: None,
            });
            current.inflight.pop_front();
            Ok(())
        })?;
        OUTBOX_NOTIFY
            .get_or_init(tokio::sync::Notify::new)
            .notify_one();
    }
}

async fn worker_loop(app: AppHandle) {
    let mut failures = 0_u32;
    let mut notified_start = false;
    loop {
        let binding = match load_binding() {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(error) => {
                *app.state::<WeixinState>().last_error.lock().unwrap() =
                    Some(format!("读取微信通道配置失败，正在重试：{error}"));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let pause = app
            .state::<WeixinState>()
            .paused_until
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|until| until.checked_duration_since(Instant::now()));
        if let Some(remaining) = pause {
            tokio::time::sleep(remaining.min(Duration::from_secs(30))).await;
            continue;
        }
        if let Err(error) = recover_inflight(&binding) {
            tracing::warn!(%error, "failed to reconcile unfinished Weixin command");
            *app.state::<WeixinState>().last_error.lock().unwrap() =
                Some(format!("恢复未完成的微信指令失败，正在重试：{error}"));
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let token = match token() {
            Ok(value) => value,
            Err(error) => {
                *app.state::<WeixinState>().last_error.lock().unwrap() =
                    Some(format!("读取微信凭据失败，正在重试：{error}"));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        if !notified_start {
            match notify_lifecycle(&binding, &token, true).await {
                Ok(()) => notified_start = true,
                Err(error) => tracing::warn!(%error, "failed to notify Weixin channel start"),
            }
        }
        let response = api_post(
            &binding.base_url,
            "ilink/bot/getupdates",
            Some(&token),
            json!({"get_updates_buf": binding.cursor, "base_info": base_info()}),
            Duration::from_secs(50),
        )
        .await;
        let response = match response {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "Weixin polling failed");
                let stale = error.contains("错误 -14");
                if stale {
                    *app.state::<WeixinState>().paused_until.lock().unwrap() =
                        Some(Instant::now() + Duration::from_secs(3600));
                }
                *app.state::<WeixinState>().last_error.lock().unwrap() = Some(if stale {
                    "微信会话暂时失效，已按服务端要求暂停连接 1 小时".into()
                } else {
                    error
                });
                failures = (failures + 1).min(6);
                let delay = if stale {
                    3600
                } else {
                    (5_u64 << (failures - 1)).min(120)
                };
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
        };
        let Some(messages) = response.get("msgs").and_then(Value::as_array) else {
            *app.state::<WeixinState>().last_error.lock().unwrap() =
                Some("微信消息响应缺少有效消息列表，正在重试".into());
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        let messages = messages.clone();
        let mut processed_all = true;
        for message in messages {
            if let Err(error) = process_message(&app, &message).await {
                tracing::warn!(%error, "Weixin message failed");
                processed_all = false;
                break;
            }
        }
        if !processed_all {
            *app.state::<WeixinState>().last_error.lock().unwrap() =
                Some("处理微信消息失败，正在重试".into());
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        if let Some(cursor) = response
            .get("get_updates_buf")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if let Err(error) = mutate_binding(|current| {
                if current.bot_id == binding.bot_id {
                    current.cursor = cursor.into();
                }
                Ok(())
            }) {
                tracing::warn!(%error, "failed to save Weixin cursor");
                *app.state::<WeixinState>().last_error.lock().unwrap() =
                    Some(format!("保存微信消息进度失败，正在重试：{error}"));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        failures = 0;
        *app.state::<WeixinState>().last_success.lock().unwrap() = Some(SystemTime::now());
        *app.state::<WeixinState>().last_error.lock().unwrap() = None;
    }
}

fn protocol_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn remember_outbound_route(binding: &mut Binding, message_id: String, session_id: String) {
    binding.outbound_routes.retain(|(id, _)| id != &message_id);
    binding.outbound_routes.push_back((message_id, session_id));
    while binding.outbound_routes.len() > MAX_ROUTES {
        binding.outbound_routes.pop_front();
    }
}

fn reply_context<'a>(message: &'a OutboundText, binding: &'a Binding) -> Option<&'a String> {
    message
        .context_token
        .as_ref()
        .or(binding.context_token.as_ref())
}

fn reconcile_bot_echo(binding: &mut Binding, message: &Value) -> bool {
    let Some(client_id) = message.get("client_id").and_then(Value::as_str) else {
        return false;
    };
    let Some((_, session_id)) = binding
        .outbound_pending
        .iter()
        .find(|(id, _)| id == client_id)
        .cloned()
    else {
        return false;
    };
    if let Some(message_id) = message.get("message_id").and_then(protocol_id) {
        remember_outbound_route(binding, message_id, session_id.clone());
    }
    if let Some(items) = message.get("item_list").and_then(Value::as_array) {
        for item in items {
            if let Some(message_id) = item.get("msg_id").and_then(protocol_id) {
                remember_outbound_route(binding, message_id, session_id.clone());
            }
        }
    }
    binding.outbound_pending.retain(|(id, _)| id != client_id);
    true
}

fn message_id(message: &Value) -> Option<String> {
    let id = |key| message.get(key).and_then(protocol_id);
    id("message_id").or_else(|| id("client_id")).or_else(|| {
        Some(format!(
            "seq:{}:{}",
            message.get("seq")?.as_u64()?,
            message.get("create_time_ms")?.as_u64()?
        ))
    })
}

fn quoted_session_marker(reference: &Value) -> Option<String> {
    let item_text = reference
        .get("message_item")
        .and_then(|item| item.get("text_item"))
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str);
    let title = reference.get("title").and_then(Value::as_str);
    [item_text, title].into_iter().flatten().find_map(|text| {
        let rest = text.trim_start().strip_prefix('#')?;
        let marker: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_hexdigit())
            .collect();
        (marker.len() == 8).then_some(marker)
    })
}

async fn process_message(app: &AppHandle, message: &Value) -> Result<(), String> {
    let mut binding = load_binding()?.ok_or("微信未绑定")?;
    if message.get("message_type").and_then(Value::as_u64) == Some(2)
        && message
            .get("client_id")
            .and_then(Value::as_str)
            .is_some_and(|client_id| {
                binding
                    .outbound_pending
                    .iter()
                    .any(|(id, _)| id == client_id)
            })
    {
        mutate_binding(|current| {
            reconcile_bot_echo(current, message);
            Ok(())
        })?;
        return Ok(());
    }
    if message.get("from_user_id").and_then(Value::as_str) != Some(binding.user_id.as_str())
        || message
            .get("group_id")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
        || message.get("message_type").and_then(Value::as_u64) != Some(1)
    {
        return Ok(());
    }
    let id = message_id(message).unwrap_or_default();
    if !id.is_empty() && binding.seen_ids.contains(&id) {
        return Ok(());
    }
    let context = message
        .get("context_token")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty());
    if let Some(context) = context {
        let bot_id = binding.bot_id.clone();
        binding = mutate_binding(|current| {
            if current.bot_id != bot_id {
                return Err("微信绑定已变化".into());
            }
            current.context_token = Some(context.into());
            Ok(())
        })?;
    }
    let mut input = String::new();
    if let Some(items) = message.get("item_list").and_then(Value::as_array) {
        for item in items {
            if let Some(value) = item
                .get("text_item")
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
            {
                input.push_str(value);
            } else if let Some(value) = item
                .get("voice_item")
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
            {
                input.push_str(value);
            }
        }
    }
    if input.len() > MAX_INPUT_CHARS {
        send_text(&binding, "消息过长，请拆分成较短的指令发送。", None).await?;
        return Ok(());
    }
    let attachments = match download_attachments(app, message).await {
        Ok(paths) => paths,
        Err(error) => {
            send_text(&binding, &format!("附件接收失败：{error}"), None).await?;
            return Ok(());
        }
    };
    if input.trim().is_empty() && attachments.is_empty() {
        send_text(
            &binding,
            "暂不支持这条消息的格式。请发送文字、语音转写、图片或常见文档。",
            None,
        )
        .await?;
        return Ok(());
    }
    let description: String = if input.trim().is_empty() {
        "附件消息".into()
    } else {
        input.trim().chars().take(80).collect()
    };
    let bot_id = binding.bot_id.clone();
    binding = mutate_binding(|current| {
        if current.bot_id != bot_id {
            return Err("微信绑定已变化".into());
        }
        if !id.is_empty() && current.seen_ids.contains(&id) {
            return Err("消息已处理".into());
        }
        if !id.is_empty() {
            if current.inflight.len() >= MAX_INFLIGHT {
                return Err("微信待确认指令过多，暂缓接收新指令".into());
            }
            current.seen_ids.push_back(id.clone());
            while current.seen_ids.len() > MAX_SEEN {
                current.seen_ids.pop_front();
            }
            current.inflight.push_back(InboundAttempt {
                message_id: id.clone(),
                description,
            });
        }
        Ok(())
    })?;
    let reference = message
        .get("item_list")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find_map(|item| item.get("ref_msg")));
    let prompt = if input.trim().is_empty() {
        "请查看我发送的附件。"
    } else {
        input.trim()
    };
    let result = route_message(app, &mut binding, prompt, reference, attachments).await;
    if let Err(error) = result {
        send_text(&binding, &format!("无法执行：{error}"), None).await?;
    }
    if !id.is_empty() {
        mutate_binding(|current| {
            current.inflight.retain(|attempt| attempt.message_id != id);
            Ok(())
        })?;
    }
    Ok(())
}

async fn route_message(
    app: &AppHandle,
    binding: &mut Binding,
    input: &str,
    reference: Option<&Value>,
    attachments: Vec<String>,
) -> Result<(), String> {
    let sessions = accessible_sessions(app, binding);
    if let Some(reference) = reference {
        let quoted_id = reference.get("svr_id").and_then(protocol_id);
        let mapped = quoted_id.as_deref().and_then(|quoted| {
            binding
                .outbound_routes
                .iter()
                .find(|(message_id, _)| message_id == quoted)
                .map(|(_, target)| target.clone())
        });
        let marked = quoted_session_marker(reference).and_then(|marker| {
            let matches: Vec<_> = sessions
                .iter()
                .filter(|session| short_id(&session.session_id).eq_ignore_ascii_case(&marker))
                .collect();
            (matches.len() == 1).then(|| matches[0].session_id.clone())
        });
        let target = mapped.or(marked);
        if quoted_id.is_some() && target.is_none() {
            return Err(
                "无法识别引用消息所属任务，请先发送“任务”并使用“切换 #编号”选择任务，再发送内容。"
                    .into(),
            );
        }
        if let Some(target) =
            target.filter(|target| sessions.iter().any(|session| session.session_id == *target))
        {
            *binding = mutate_binding(|current| {
                current.active_session = Some(target);
                Ok(())
            })?;
        } else if quoted_id.is_some() {
            return Err("引用消息所属任务已不可通过微信访问，请在桌面检查授权。".into());
        }
    }
    if attachments.is_empty() && (input == "帮助" || input == "/help") {
        return send_text(binding, "微信继续 · 常用指令\n\n• 任务 / 任务2：查看任务、翻页\n• 切换 #编号：进入已有任务\n• 状态 / 停止：查看或停止当前任务\n\n• 工作区：查看可用目录\n•「新任务：内容」：在默认工作区创建任务\n•「新任务 2：内容」：在指定工作区创建任务\n• /文件 相对路径：取回工作区文件\n\n普通消息会继续当前任务。", None).await;
    }
    if attachments.is_empty() {
        if let Some(page) = task_page_command(input)? {
            let text = format_task_page(&sessions, binding.active_session.as_deref(), page)?;
            return send_text(binding, &text, None).await;
        }
    }
    if let Some(target) = attachments
        .is_empty()
        .then(|| {
            input
                .strip_prefix("切换 ")
                .or_else(|| input.strip_prefix("/switch "))
        })
        .flatten()
    {
        let target = target.trim().trim_start_matches('#');
        let matches: Vec<_> = sessions
            .iter()
            .filter(|s| {
                s.session_id == target || short_id(&s.session_id).eq_ignore_ascii_case(target)
            })
            .collect();
        if matches.len() != 1 {
            return Err("未找到唯一会话，请发送“任务”查看编号".into());
        }
        *binding = mutate_binding(|current| {
            current.active_session = Some(matches[0].session_id.clone());
            Ok(())
        })?;
        return send_text(
            binding,
            &format!(
                "已切换到 #{}《{}》",
                short_id(&matches[0].session_id),
                matches[0].title
            ),
            Some(&matches[0].session_id),
        )
        .await;
    }
    if attachments.is_empty() && (input == "状态" || input == "/status") {
        let current = sessions
            .iter()
            .find(|s| Some(&s.session_id) == binding.active_session.as_ref())
            .ok_or("尚未选择任务，请发送“任务”")?;
        return send_text(
            binding,
            &format!(
                "当前任务 #{}《{}》\n工作区：{}\n状态：{}",
                short_id(&current.session_id),
                current.title,
                current.cwd,
                current.status.as_deref().unwrap_or("未知")
            ),
            Some(&current.session_id),
        )
        .await;
    }
    if attachments.is_empty() && (input == "工作区" || input == "/workspaces") {
        let text = if binding.allowed_workspaces.is_empty() {
            "尚未授权工作区，请在桌面“设置 → 消息通道”中勾选。".into()
        } else {
            format!(
                "可用于新任务的工作区：\n{}",
                binding
                    .allowed_workspaces
                    .iter()
                    .enumerate()
                    .map(|(index, cwd)| format!(
                        "{}. {}{}",
                        index + 1,
                        cwd,
                        if default_workspace(binding) == Some(cwd.as_str()) {
                            "（默认）"
                        } else {
                            ""
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        return send_text(binding, &text, None).await;
    }
    if attachments.is_empty() && (input == "停止" || input == "/stop") {
        let current = sessions
            .iter()
            .find(|s| Some(&s.session_id) == binding.active_session.as_ref())
            .ok_or("尚未选择任务")?;
        crate::commands::agent_cancel(
            app.state::<AppState>(),
            current.session_id.clone(),
            Some("stop".into()),
            None,
        )
        .await?;
        return send_text(
            binding,
            &format!(
                "已请求停止 #{}《{}》",
                short_id(&current.session_id),
                current.title
            ),
            Some(&current.session_id),
        )
        .await;
    }
    if let Some(relative) = attachments
        .is_empty()
        .then(|| {
            input
                .strip_prefix("/文件 ")
                .or_else(|| input.strip_prefix("/file "))
        })
        .flatten()
    {
        let current = sessions
            .iter()
            .find(|s| Some(&s.session_id) == binding.active_session.as_ref())
            .ok_or("尚未选择任务")?;
        send_workspace_file(app, binding, &current.session_id, &current.cwd, relative).await?;
        return send_text(
            binding,
            &format!(
                "已发送 #{} 的文件：{}",
                short_id(&current.session_id),
                relative.trim()
            ),
            Some(&current.session_id),
        )
        .await;
    }
    if let Some((verb, rest)) = attachments
        .is_empty()
        .then(|| input.split_once(' '))
        .flatten()
    {
        let code = rest.split_whitespace().next().unwrap_or("").to_uppercase();
        if matches!(verb, "允许" | "拒绝" | "通过" | "回答") {
            return resolve_interaction(app, binding, verb, &code, rest).await;
        }
    }
    let default_prompt = input
        .strip_prefix("新任务：")
        .or_else(|| input.strip_prefix("新任务:"))
        .or_else(|| input.strip_prefix("/new "));
    let numbered_prompt = input
        .strip_prefix("新任务 ")
        .and_then(|rest| rest.split_once('：').or_else(|| rest.split_once(':')))
        .and_then(|(number, prompt)| {
            number
                .trim()
                .parse::<usize>()
                .ok()
                .map(|index| (index, prompt))
        });
    if input.starts_with("新任务 ") && numbered_prompt.is_none() {
        return Err("请使用“新任务 2：内容”并填写有效工作区编号".into());
    }
    if let Some((workspace_index, prompt)) = default_prompt
        .map(|prompt| (None, prompt))
        .or_else(|| numbered_prompt.map(|(index, prompt)| (Some(index), prompt)))
    {
        if workspace_index == Some(0) {
            return Err("工作区编号从 1 开始".into());
        }
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("新任务内容不能为空".into());
        }
        let cwd = match workspace_index {
            Some(index) => binding.allowed_workspaces.get(index - 1).cloned(),
            None => default_workspace(binding).map(str::to_string),
        }
        .ok_or("工作区编号无效，请发送“工作区”查看可用目录")?;
        ensure_runtime(app).await?;
        let session_id = crate::commands::agent_new_session(
            app.clone(),
            app.state::<AppState>(),
            cwd,
            None,
            Some("ask".into()),
        )
        .await?;
        *binding = mutate_binding(|current| {
            current.active_session = Some(session_id.clone());
            Ok(())
        })?;
        send_text(
            binding,
            &format!("已创建任务 #{}，正在执行。", short_id(&session_id)),
            Some(&session_id),
        )
        .await?;
        app.state::<WeixinState>()
            .progress_at
            .lock()
            .unwrap()
            .insert(session_id.clone(), Instant::now());
        return crate::commands::agent_send(
            app.clone(),
            app.state::<AppState>(),
            session_id,
            prompt.into(),
            Some(attachments),
            None,
            Some(Uuid::now_v7().to_string()),
            Some(false),
            None,
            None,
            None,
        )
        .await;
    }
    let current = sessions
        .iter()
        .find(|s| Some(&s.session_id) == binding.active_session.as_ref())
        .ok_or("尚未选择任务，请发送“任务”选择已有会话，或发送“新任务：...”")?;
    ensure_runtime(app).await?;
    if app
        .state::<AppState>()
        .session_workspace(&current.session_id)
        .is_err()
    {
        crate::commands::agent_load_session(
            app.clone(),
            app.state::<AppState>(),
            current.session_id.clone(),
            current.cwd.clone(),
        )
        .await?;
    }
    let prompt_id = Uuid::now_v7().to_string();
    app.state::<WeixinState>()
        .progress_at
        .lock()
        .unwrap()
        .insert(current.session_id.clone(), Instant::now());
    crate::commands::agent_send(
        app.clone(),
        app.state::<AppState>(),
        current.session_id.clone(),
        input.into(),
        Some(attachments),
        None,
        Some(prompt_id),
        Some(false),
        None,
        None,
        None,
    )
    .await?;
    send_text(
        binding,
        &format!(
            "已送达 #{}《{}》",
            short_id(&current.session_id),
            current.title
        ),
        Some(&current.session_id),
    )
    .await
}

async fn ensure_runtime(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let init = crate::commands::agent_init(
        app.clone(),
        state,
        app.state::<crate::bridge::Permissions>(),
        app.state::<crate::bridge::Questions>(),
        app.state::<crate::bridge::PlanApprovals>(),
        app.state::<crate::bridge::FolderTrusts>(),
        None,
    )
    .await?;
    if init.ok {
        Ok(())
    } else {
        Err("Agent 尚未就绪，请打开桌面检查模型连接".into())
    }
}

async fn resolve_interaction(
    app: &AppHandle,
    binding: &Binding,
    verb: &str,
    code: &str,
    rest: &str,
) -> Result<(), String> {
    let pending = app
        .state::<WeixinState>()
        .pending
        .lock()
        .unwrap()
        .get(code)
        .cloned()
        .ok_or("确认编号不存在或已过期")?;
    if !accessible_sessions(app, binding)
        .iter()
        .any(|s| s.session_id == pending.session_id)
    {
        return Err("此会话未授权微信访问".into());
    }
    let resolved = match pending.kind {
        PendingKind::Permission { allow, deny } => {
            let option = match verb {
                "允许" => allow.ok_or("操作参数未能在微信完整展示，请在桌面审阅后批准")?,
                "拒绝" => deny.ok_or("此操作不支持微信拒绝")?,
                _ => return Err("此确认只支持“允许”或“拒绝”".into()),
            };
            crate::commands::agent_resolve_permission(
                app.clone(),
                app.state::<crate::bridge::Permissions>(),
                pending.request_id,
                Some(option),
                Some(false),
            )
            .await?
        }
        PendingKind::Question { questions } => {
            if verb != "回答" {
                return Err("请用“回答 编号 内容”回复问题".into());
            }
            let answer = rest
                .trim()
                .split_once(' ')
                .map(|(_, value)| value.trim())
                .unwrap_or("");
            if answer.is_empty() {
                return Err("回答内容不能为空".into());
            }
            let values = if questions.len() == 1 {
                vec![answer.to_string()]
            } else {
                let mut values = vec![String::new(); questions.len()];
                for pair in answer.split(';') {
                    let (number, value) = pair
                        .split_once('=')
                        .ok_or("请按“1=答案;2=答案”填写每个问题")?;
                    let index = number.trim().parse::<usize>().map_err(|_| "问题编号无效")?;
                    if index == 0 || index > questions.len() {
                        return Err("问题编号无效".into());
                    }
                    values[index - 1] = value.trim().into();
                }
                if values.iter().any(|value| value.is_empty()) {
                    return Err("请回答每个问题".into());
                }
                values
            };
            let answers = questions
                .iter()
                .map(|question| (question.clone(), json!("Other")))
                .collect();
            let annotations = questions
                .into_iter()
                .zip(values)
                .map(|(question, value)| {
                    (
                        question,
                        crate::commands::QuestionAnnotationDto {
                            preview: None,
                            notes: Some(value),
                        },
                    )
                })
                .collect();
            crate::commands::agent_resolve_question(
                app.state::<crate::bridge::Questions>(),
                pending.request_id,
                Some(answers),
                Some(annotations),
                None,
                Some("accepted".into()),
                Some(false),
            )
            .await?
        }
        PendingKind::Plan { can_approve } => {
            if verb == "通过" && !can_approve {
                return Err("方案详情不完整，请在桌面审阅后批准".into());
            }
            let outcome = match verb {
                "通过" => "approved",
                "拒绝" => "cancelled",
                _ => return Err("请用“通过”或“拒绝”回复计划".into()),
            };
            crate::commands::agent_resolve_plan_approval(
                app.state::<crate::bridge::PlanApprovals>(),
                pending.request_id,
                outcome.into(),
                None,
            )
            .await?
        }
    };
    app.state::<WeixinState>()
        .pending
        .lock()
        .unwrap()
        .remove(code);
    send_text(
        binding,
        if resolved {
            "确认已处理。"
        } else {
            "确认已在桌面或其他设备处理。"
        },
        Some(&pending.session_id),
    )
    .await
}

fn outbound_segments(text: &str, session_id: Option<&str>) -> Vec<String> {
    let label = session_id.map(|id| format!("#{}", short_id(id)));
    let chars: Vec<char> = text.chars().collect();
    let chunks: Vec<String> = chars
        .chunks(MAX_REPLY_CHARS - 80)
        .map(|chunk| chunk.iter().collect())
        .collect();
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut body = String::new();
            let mut content = chunk.as_str();
            if let Some(label) = label.as_deref() {
                body.push_str(label);
                body.push('\n');
                if index == 0 {
                    if let Some(rest) = content.strip_prefix(label) {
                        content = rest.trim_start_matches('\n');
                    }
                }
            }
            if chunks.len() > 1 {
                body.push_str(&format!("（{}/{}）\n", index + 1, chunks.len()));
            }
            body.push_str(content);
            body
        })
        .collect()
}

fn queue_text(
    binding: &Binding,
    text: &str,
    session_id: Option<&str>,
    request_code: Option<&str>,
) -> Result<(), String> {
    let segments = outbound_segments(text, session_id);
    if segments.is_empty() {
        return Ok(());
    }
    mutate_binding(|current| {
        if current.bot_id != binding.bot_id || current.user_id != binding.user_id {
            return Err("微信绑定已变化".into());
        }
        if current.outbox.len() + segments.len() > MAX_OUTBOX {
            return Err("微信待发送消息过多，请恢复网络后重试".into());
        }
        for text in segments {
            current.outbox.push_back(OutboundText {
                client_id: Uuid::now_v7().to_string(),
                text,
                session_id: session_id.map(str::to_string),
                context_token: current.context_token.clone(),
                request_code: request_code.map(str::to_string),
            });
        }
        Ok(())
    })?;
    OUTBOX_NOTIFY
        .get_or_init(tokio::sync::Notify::new)
        .notify_one();
    Ok(())
}

async fn send_text(binding: &Binding, text: &str, session_id: Option<&str>) -> Result<(), String> {
    queue_text(binding, text, session_id, None)
}

async fn send_interaction_text(
    binding: &Binding,
    text: &str,
    session_id: &str,
    request_code: &str,
) -> Result<(), String> {
    queue_text(binding, text, Some(session_id), Some(request_code))
}

#[cfg(test)]
mod tests {
    #[test]
    fn unbound_shutdown_never_reads_system_credentials() {
        for binding in [Ok(None), Err("invalid binding".to_string())] {
            assert!(
                super::shutdown_identity(binding, || panic!("unexpected credential access"))
                    .is_none()
            );
        }
        let binding = super::Binding {
            bot_id: "owned-bot".into(),
            ..Default::default()
        };
        let identity =
            super::shutdown_identity(Ok(Some(binding)), || Ok("synthetic-credential".into()))
                .unwrap();
        assert_eq!(identity.0.bot_id, "owned-bot");
        assert_eq!(identity.1, "synthetic-credential");
        assert!(
            super::shutdown_identity(Ok(Some(super::Binding::default())), || Err("locked".into()))
                .is_none()
        );
    }

    use super::{
        cdn_url, decrypt_media, default_workspace, encrypt_media, format_task_page, message_id,
        outbound_segments, quoted_session_marker, reconcile_bot_echo, reply_context,
        reviewable_permission_details, revoke_session_access, task_page_command, validate_base_url,
        Binding, InboundAttempt, OutboundText,
    };
    use crate::sessions::SessionSummary;
    use aes::cipher::{BlockEncrypt, KeyInit};
    use base64::Engine;
    use serde_json::json;

    #[test]
    fn task_pages_accept_compact_and_spaced_commands() {
        assert_eq!(task_page_command("任务").unwrap(), Some(1));
        assert_eq!(task_page_command("任务2").unwrap(), Some(2));
        assert_eq!(task_page_command("任务 2").unwrap(), Some(2));
        assert_eq!(task_page_command("/sessions 2").unwrap(), Some(2));
        assert_eq!(task_page_command("任务完成了吗").unwrap(), None);
        assert!(task_page_command("任务0").is_err());
        assert!(task_page_command("任务 2x").is_err());
    }

    #[test]
    fn task_page_is_scannable_and_shows_usable_navigation() {
        let sessions: Vec<SessionSummary> = (0..7)
            .map(|index| {
                serde_json::from_value(json!({
                    "sessionId": format!("session-{index:08}"),
                    "title": if index == 0 { "修复\n 一个 任务".to_string() } else { format!("任务 {index}") },
                    "cwd": "/repo/日常",
                    "permissionMode": "ask"
                }))
                .unwrap()
            })
            .collect();
        let first = format_task_page(&sessions, Some(&sessions[0].session_id), 1).unwrap();
        assert!(first.contains("可继续的任务 · 1/2（共 7 个）"));
        assert!(first.contains("• 【当前】修复 一个 任务\n  #00000000 · 日常"));
        assert_eq!(first.matches("• ").count(), 5);
        assert!(first.contains("下一页：发送“任务2”"));
        assert!(first.contains("切换任务：发送“切换 #编号”"));

        let second = format_task_page(&sessions, None, 2).unwrap();
        assert_eq!(second.matches("• ").count(), 2);
        assert!(second.contains("上一页：发送“任务1”"));
        assert!(!second.contains("下一页："));
    }

    #[test]
    fn remote_permission_requires_complete_parameters() {
        assert_eq!(
            reviewable_permission_details(Some(&json!({"path": "notes.txt"}))),
            Some("{\"path\":\"notes.txt\"}".into())
        );
        assert!(reviewable_permission_details(None).is_none());
        assert!(reviewable_permission_details(Some(&json!(null))).is_none());
        assert!(reviewable_permission_details(Some(
            &json!({"_truncated": true, "message": "omitted"})
        ))
        .is_none());
        assert!(
            reviewable_permission_details(Some(&json!({"content": "x".repeat(2500)}))).is_none()
        );
    }

    #[test]
    fn quoted_reply_can_recover_session_without_server_message_id() {
        let session_id = "0199f962-73ed-72ff-a1bb-12345678";
        let segments = outbound_segments(&"好".repeat(4000), Some(session_id));
        assert_eq!(segments.len(), 2);
        assert!(segments
            .iter()
            .all(|segment| segment.starts_with("#12345678\n")));
        let already_labeled = outbound_segments(
            &format!("#12345678《任务》\n{}", "好".repeat(4000)),
            Some(session_id),
        );
        assert!(already_labeled
            .iter()
            .all(|segment| segment.starts_with("#12345678\n")));
        let reference = json!({"message_item": {"text_item": {"text": segments[1]}}});
        assert_eq!(
            quoted_session_marker(&reference).as_deref(),
            Some("12345678")
        );
        assert_eq!(
            quoted_session_marker(&json!({"title": "#12345678《任务》"})).as_deref(),
            Some("12345678")
        );
    }

    #[test]
    fn queued_reply_and_inbound_attempt_survive_serialization() {
        let mut binding = Binding {
            bot_id: "bot".into(),
            user_id: "user".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            ..Binding::default()
        };
        binding.outbox.push_back(OutboundText {
            client_id: "stable-id".into(),
            text: "reply".into(),
            session_id: Some("session".into()),
            context_token: Some("context".into()),
            request_code: None,
        });
        binding.inflight.push_back(InboundAttempt {
            message_id: "inbound-id".into(),
            description: "创建任务".into(),
        });
        let restored: Binding =
            serde_json::from_slice(&serde_json::to_vec(&binding).unwrap()).unwrap();
        assert_eq!(restored.outbox[0].client_id, "stable-id");
        assert_eq!(restored.inflight[0].message_id, "inbound-id");
        assert_eq!(
            message_id(&json!({"client_id": "fallback"})).as_deref(),
            Some("fallback")
        );
        assert_eq!(
            message_id(&json!({"message_id": null, "client_id": "fallback"})).as_deref(),
            Some("fallback")
        );
        assert_eq!(
            message_id(&json!({"message_id": "", "seq": 12, "create_time_ms": 34})).as_deref(),
            Some("seq:12:34")
        );
    }

    #[test]
    fn default_workspace_migrates_and_delayed_reply_keeps_its_context() {
        let mut binding = Binding {
            allowed_workspaces: vec!["/repo/A".into(), "/repo/B".into()],
            context_token: Some("latest".into()),
            ..Binding::default()
        };
        assert_eq!(default_workspace(&binding), Some("/repo/A"));
        binding.default_workspace = Some("/repo/B".into());
        assert_eq!(default_workspace(&binding), Some("/repo/B"));
        let queued = OutboundText {
            client_id: "id".into(),
            text: "reply".into(),
            session_id: None,
            context_token: Some("original".into()),
            request_code: None,
        };
        assert_eq!(
            reply_context(&queued, &binding).map(String::as_str),
            Some("original")
        );
    }

    #[test]
    fn bot_echo_maps_id_only_quotes_to_the_sent_session() {
        let mut binding = Binding::default();
        binding
            .outbound_pending
            .push_back(("client-1".into(), "session-1".into()));
        let echo = json!({
            "client_id": "client-1", "message_id": 18446744073709551615_u64,
            "item_list": [{"msg_id": 18446744073709551614_u64}]
        });
        assert!(reconcile_bot_echo(&mut binding, &echo));
        assert!(binding.outbound_pending.is_empty());
        assert!(binding
            .outbound_routes
            .iter()
            .any(|(id, target)| id == "18446744073709551615" && target == "session-1"));
        assert!(binding
            .outbound_routes
            .iter()
            .any(|(id, target)| id == "18446744073709551614" && target == "session-1"));
    }

    #[test]
    fn revoking_handoff_clears_active_session_and_queued_reply() {
        let mut binding = Binding {
            active_session: Some("session".into()),
            shared_sessions: vec!["session".into()],
            ..Binding::default()
        };
        binding.outbox.push_back(OutboundText {
            client_id: "queued".into(),
            text: "reply".into(),
            session_id: Some("session".into()),
            context_token: None,
            request_code: None,
        });
        revoke_session_access(&mut binding, "session", false).unwrap();
        assert!(binding.active_session.is_none());
        assert!(binding.shared_sessions.is_empty());
        assert!(binding.outbox.is_empty());
    }

    #[test]
    fn revoking_redundant_handoff_keeps_active_workspace_session() {
        let mut binding = Binding {
            active_session: Some("session".into()),
            shared_sessions: vec!["session".into()],
            allowed_workspaces: vec!["/repo".into()],
            ..Binding::default()
        };
        binding.outbox.push_back(OutboundText {
            client_id: "queued".into(),
            text: "reply".into(),
            session_id: Some("session".into()),
            context_token: None,
            request_code: None,
        });
        revoke_session_access(&mut binding, "session", true).unwrap();
        assert_eq!(binding.active_session.as_deref(), Some("session"));
        assert_eq!(binding.outbox.len(), 1);
        assert!(binding.shared_sessions.is_empty());
    }

    #[test]
    fn rejects_untrusted_api_and_media_hosts() {
        assert!(validate_base_url("https://ilinkai.weixin.qq.com").is_ok());
        assert!(validate_base_url("http://ilinkai.weixin.qq.com").is_err());
        assert!(validate_base_url("https://ilinkai.weixin.qq.com.evil.test").is_err());
        assert!(cdn_url(
            &json!({"full_url": "https://novac2c.cdn.weixin.qq.com/c2c/download?x=1"})
        )
        .is_ok());
        assert!(cdn_url(&json!({"full_url": "https://example.com/private"})).is_err());
    }

    #[test]
    #[allow(deprecated)]
    fn decrypts_weixin_media_and_rejects_invalid_padding() {
        let key = [7_u8; 16];
        let cipher = aes::Aes128::new_from_slice(&key).unwrap();
        let mut plaintext = b"hello Weixin".to_vec();
        plaintext.extend(std::iter::repeat_n(4_u8, 4));
        let mut encrypted = plaintext.clone();
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            &mut encrypted,
        ));
        let key64 = base64::engine::general_purpose::STANDARD.encode(key);
        assert_eq!(
            decrypt_media(encrypted.clone(), &key64, false).unwrap(),
            b"hello Weixin"
        );
        assert_eq!(
            decrypt_media(
                encrypt_media(b"round trip".to_vec(), &key).unwrap(),
                &key64,
                false
            )
            .unwrap(),
            b"round trip"
        );
        let mut invalid = [0_u8; 16];
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            &mut invalid,
        ));
        assert!(decrypt_media(invalid.to_vec(), &key64, false).is_err());
    }
}
