//! Built-in, task-scoped document creation for the embedded Agent Runtime.
//! The model supplies content and format; the destination comes from the
//! authoritative session workspace; explicit paths stay inside that workspace.

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub const MCP_SERVER_NAME: &str = "echoagent-office";
pub const SESSION_HEADER: &str = "X-EchoAgent-Session-Id";
const BODY_LIMIT: usize = 5 * 1024 * 1024;
static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
static PERSISTED: Mutex<bool> = Mutex::new(false);
type CreationLock = tokio::sync::Mutex<()>;
static CREATIONS: OnceLock<Mutex<HashMap<String, Weak<CreationLock>>>> = OnceLock::new();

fn creation_lock(session: &str) -> Result<Arc<CreationLock>, String> {
    let mut locks = CREATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "文档创建锁已损坏")?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(session).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(CreationLock::new(()));
    locks.insert(session.into(), Arc::downgrade(&lock));
    Ok(lock)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateArguments {
    #[serde(flatten)]
    document: crate::document_export::DocumentExportRequest,
    output_path: Option<String>,
    request_id: Option<String>,
}

fn creation_digest(
    request: &crate::document_export::DocumentExportRequest,
    explicit_destination: Option<&Path>,
) -> Result<String, String> {
    // Model retries may reorder JSON object keys. Hash canonical content so
    // object field order cannot turn a safe retry into a content conflict.
    let mut value =
        serde_json::to_value((request, explicit_destination)).map_err(|e| e.to_string())?;
    value.sort_all_objects();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).map_err(|e| e.to_string())?)
    ))
}

struct Service {
    port: u16,
    token: String,
    ready: AtomicBool,
}

#[derive(Clone)]
struct ServerState {
    app: AppHandle,
    authorization: String,
    expected_host: String,
}

#[derive(Deserialize)]
struct RpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn serve(app: AppHandle) -> Result<(), String> {
    if SERVICE.get().is_some() {
        return Ok(());
    }
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|error| format!("办公文档服务无法绑定本地端口：{error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let service = Arc::new(Service {
        port,
        token: uuid::Uuid::now_v7().to_string(),
        ready: AtomicBool::new(false),
    });
    let state = ServerState {
        app,
        authorization: format!("Bearer {}", service.token),
        expected_host: format!("127.0.0.1:{port}"),
    };
    SERVICE
        .set(service.clone())
        .map_err(|_| "办公文档服务重复启动".to_string())?;
    let (startup_tx, startup_rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                service.ready.store(false, Ordering::Release);
                tracing::error!(%error, "office MCP listener failed");
                let _ = startup_tx.send(Err(format!("办公文档服务启动失败：{error}")));
                return;
            }
        };
        let router = Router::new()
            .route("/mcp", post(handle_post))
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(state);
        service.ready.store(true, Ordering::Release);
        let _ = startup_tx.send(Ok(()));
        if let Err(error) = axum::serve(listener, router).await {
            tracing::error!(%error, "office MCP server stopped");
        }
        service.ready.store(false, Ordering::Release);
        *PERSISTED.lock().unwrap() = false;
    });
    startup_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| format!("等待办公文档服务启动超时：{error}"))?
}

pub fn server_url() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("http://127.0.0.1:{}/mcp", service.port))
    })
}

pub fn authorization_header() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("Bearer {}", service.token))
    })
}

fn valid_headers(headers: &HeaderMap, state: &ServerState) -> Result<String, StatusCode> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // The secret is generated for this process, so comparison is constant time.
    if auth.len() != state.authorization.len()
        || auth
            .as_bytes()
            .iter()
            .zip(state.authorization.as_bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            != 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(state.expected_host.as_str())
    {
        return Err(StatusCode::MISDIRECTED_REQUEST);
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let session_id = headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if session_id.is_empty() || session_id.len() > 256 || session_id.chars().any(char::is_control) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(session_id.to_string())
}

async fn handle_post(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let session_id = match valid_headers(&headers, &state) {
        Ok(session_id) => session_id,
        Err(status) => return status.into_response(),
    };
    if body.len() > BODY_LIMIT {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let request: RpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => return rpc_error(Value::Null, -32700, format!("invalid JSON: {error}")),
    };
    let Some(id) = request.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    if !matches!(id, Value::String(_) | Value::Number(_)) {
        return rpc_error(Value::Null, -32600, "invalid JSON-RPC id".into());
    }
    let result = match request.method.as_str() {
        "initialize" => json!({
            "protocolVersion": request.params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-03-26"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": MCP_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => office_tools(),
        "tools/call" => match call_tool(&state.app, &session_id, &request.params).await {
            Ok(receipt) => json!({ "content": [{ "type": "text", "text": receipt.to_string() }] }),
            Err(error) => {
                json!({ "content": [{ "type": "text", "text": error }], "isError": true })
            }
        },
        other => return rpc_error(id, -32601, format!("unknown method: {other}")),
    };
    rpc_result(id, result)
}

async fn call_tool(app: &AppHandle, session_id: &str, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if matches!(name, "office_document_read" | "office_document_update") {
        let args = params.get("arguments").ok_or("缺少文档参数")?;
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or("缺少文档路径")?;
        if name == "office_document_read" {
            let opened = crate::document_workspace::open_for_session(app, session_id, path).await?;
            let doc = opened
                .document
                .ok_or("该文件尚无可编辑稿，请先在成果面板中创建可编辑副本")?;
            return Ok(
                json!({"path":doc.path,"title":doc.title,"revision":doc.revision,
                "exportedRevision":doc.exported_revision,"externalChange":doc.external_change,"document":doc.document}),
            );
        }
        let revision = args
            .get("expectedRevision")
            .and_then(Value::as_u64)
            .ok_or("必须提供读取时的 expectedRevision")?;
        let document = args.get("document").cloned().ok_or("缺少文档内容")?;
        let saved = crate::document_workspace::save_for_session(
            app,
            session_id,
            path,
            revision,
            document,
            "Agent 修改",
        )
        .await?;
        let published =
            crate::document_workspace::publish_for_session(app, session_id, path, saved.revision)
                .await?;
        return Ok(
            json!({"path":published.path,"format":"docx","byteSize":published.byte_size,
            "sha256":published.file_sha256,"revision":published.revision}),
        );
    }
    if params.get("name").and_then(Value::as_str) != Some("office_create") {
        return Err("未知的办公文档工具".into());
    }
    let arguments: CreateArguments =
        serde_json::from_value(params.get("arguments").cloned().ok_or("缺少文档参数")?)
            .map_err(|error| format!("文档参数无效：{error}"))?;
    let request = arguments.document;
    crate::document_export::validate_request(&request)?;
    // Use the live native session binding. Historical listings are capped and
    // can omit an older task even while it is open in the desktop.
    let session_cwd = app
        .state::<crate::commands::AppState>()
        .session_workspace(session_id)?;
    let access = app.state::<crate::shell_fs::FilesystemAccess>();
    let workspace = access.require_workspace(&session_cwd.to_string_lossy())?;
    let filename = format!(
        "{}-{}.{}",
        crate::document_export::safe_file_title(&request.title),
        uuid::Uuid::now_v7().simple(),
        request.format,
    );
    let requested = arguments
        .output_path
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("EchoAgent成果/{filename}"));
    let lock = creation_lock(session_id)?;
    let _guard = lock.lock().await;
    let destination = creation_destination(&workspace, &requested, &request.format)?;
    if arguments
        .request_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
    {
        return Err("requestId 应为 1–128 字符".into());
    }
    let key = arguments
        .request_id
        .as_ref()
        .map(|id| format!("id:{id}"))
        .or_else(|| {
            arguments
                .output_path
                .as_ref()
                .map(|_| destination.to_string_lossy().into_owned())
        });
    let digest = creation_digest(
        &request,
        arguments
            .output_path
            .as_ref()
            .map(|_| destination.as_path()),
    )?;
    if let Some(key) = &key {
        if let Some(receipt) = crate::session_artifacts::creation_receipt(session_id, key, &digest)?
        {
            return serde_json::to_value(receipt).map_err(|e| e.to_string());
        }
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(
            "目标文件已存在，未覆盖。请指定新的 outputPath；同一次生成重试请使用相同 requestId"
                .into(),
        );
    }
    let receipt = crate::document_export::create_for_session(
        app,
        request,
        &destination,
        session_id,
        key.as_deref().map(|k| (k, digest.as_str())),
    )
    .await?;
    serde_json::to_value(receipt).map_err(|error| error.to_string())
}

pub(crate) fn creation_destination(
    workspace: &Path,
    requested: &str,
    format: &str,
) -> Result<PathBuf, String> {
    // FilesystemAccess uses std::canonicalize, which returns verbatim paths
    // on Windows. Compare both sides in the same representation.
    let workspace =
        dunce::canonicalize(workspace).map_err(|e| format!("无法确认任务工作区：{e}"))?;
    let workspace = workspace.as_path();
    if requested.is_empty() || requested.len() > 4096 || requested.chars().any(char::is_control) {
        return Err("输出路径无效".into());
    }
    let raw = Path::new(requested);
    if raw.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("输出路径不能包含上级目录".into());
    }
    let mut destination = if raw.is_absolute() {
        dunce::simplified(raw).to_path_buf()
    } else {
        workspace.join(raw)
    };
    if destination.extension().is_none() {
        destination.set_extension(format);
    }
    if !destination
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(format))
    {
        return Err("输出文件扩展名与格式不一致".into());
    }
    let relative = destination
        .strip_prefix(workspace)
        .map_err(|_| "输出位置不在当前任务工作区，请先通过目录选择器授权并切换到目标工作区")?;
    let name = relative
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("无效文件名")?;
    if name
        .chars()
        .any(|c| matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || name.ends_with(['.', ' '])
    {
        return Err("输出文件名包含不支持的字符".into());
    }
    let mut parent = workspace.to_path_buf();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        let Component::Normal(part) = component else {
            return Err("输出目录无效".into());
        };
        parent.push(part);
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err("输出路径包含符号链接或非目录".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&parent).map_err(|e| format!("无法创建输出目录：{e}"))?;
            }
            Err(e) => return Err(e.to_string()),
        }
        if !dunce::canonicalize(&parent)
            .map_err(|e| e.to_string())?
            .starts_with(workspace)
        {
            return Err("输出目录超出任务工作区".into());
        }
    }
    Ok(dunce::canonicalize(parent)
        .map_err(|e| e.to_string())?
        .join(name))
}

fn office_tools() -> Value {
    static TOOLS: OnceLock<Value> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            serde_json::from_str(include_str!("office-tools.json")).expect("bundled Office schema")
        })
        .clone()
}

fn rpc_result(id: Value, result: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, message: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string(),
    )
        .into_response()
}

pub fn persist_registration(tx: &echo_agent_acp::AcpAgentTx, session_id: &str) {
    {
        let mut persisted = PERSISTED.lock().unwrap();
        if *persisted {
            return;
        }
        *persisted = true;
    }
    let (Some(url), Some(authorization)) = (server_url(), authorization_header()) else {
        *PERSISTED.lock().unwrap() = false;
        return;
    };
    let tx = tx.clone();
    let session_id = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let payload = json!({
            "session_id": session_id,
            "server_name": MCP_SERVER_NAME,
            "url": url,
            "headers": {
                "Authorization": authorization,
                (SESSION_HEADER): "${session_id}"
            },
            "enabled": true,
        });
        if let Err(error) = crate::ext::call_ext_value(
            &tx,
            "echo.agent/mcp/upsert",
            crate::ext::raw_params(&payload),
        )
        .await
        {
            tracing::warn!(?error, "office MCP registration failed");
            *PERSISTED.lock().unwrap() = false;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths_create_missing_parents_without_escaping_workspace() {
        let root = tempfile::tempdir().unwrap();
        // Match the actual FilesystemAccess return value, including \\?\ on Windows.
        let workspace = std::fs::canonicalize(root.path()).unwrap();
        let expected = dunce::canonicalize(root.path()).unwrap();
        let path =
            creation_destination(&workspace, "output/new/deep/monthly.xlsx", "xlsx").unwrap();
        assert_eq!(path, expected.join("output/new/deep/monthly.xlsx"));
        assert!(path.parent().unwrap().is_dir());
        for absolute in [
            expected.join("output/absolute.xlsx"),
            workspace.join("output/native.xlsx"),
        ] {
            assert_eq!(
                creation_destination(&workspace, absolute.to_str().unwrap(), "xlsx").unwrap(),
                dunce::simplified(&absolute),
            );
        }
        assert!(creation_destination(&workspace, "EchoAgent成果/default.xlsx", "xlsx").is_ok());
        for bad in [
            "../outside.xlsx",
            "output/../../outside.xlsx",
            "output/a.docx",
            "output/secret\n.xlsx",
            "output/a?.xlsx",
        ] {
            assert!(
                creation_destination(&workspace, bad, "xlsx").is_err(),
                "{bad}"
            );
        }
        let other = tempfile::tempdir().unwrap();
        assert!(creation_destination(
            &workspace,
            &other.path().join("other.xlsx").to_string_lossy(),
            "xlsx"
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn output_symlinks_are_rejected_before_creating_any_external_directory() {
        let root = tempfile::tempdir().unwrap();
        let workspace = dunce::canonicalize(root.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.join("escape")).unwrap();
        assert!(creation_destination(&workspace, "escape/new/a.xlsx", "xlsx").is_err());
        assert!(!outside.path().join("new").exists());
    }

    #[test]
    fn structured_arguments_and_schema_keep_legacy_markdown_compatible() {
        for value in [
            json!({"title":"报告","format":"docx","markdown":"# 报告"}),
            json!({"title":"表格","format":"xlsx","outputPath":"output/monthly.xlsx","requestId":"r1","workbook":{"sheets":[{"name":"指标","rows":[["收入"],[1]]}]}}),
            json!({"title":"汇报","format":"pptx","slides":[{"title":"第一页"}]}),
        ] {
            let arguments: CreateArguments = serde_json::from_value(value).unwrap();
            crate::document_export::validate_request(&arguments.document).unwrap();
        }
        let tools = office_tools();
        let properties = &tools["tools"][0]["inputSchema"]["properties"];
        assert!(properties.get("outputPath").is_some());
        assert!(properties.get("workbook").is_some());
        assert!(properties.get("slides").is_some());
    }

    #[test]
    fn retry_identity_ignores_object_key_order_but_preserves_content_and_destination() {
        let first: CreateArguments = serde_json::from_str(r#"{"title":"表格","format":"xlsx","workbook":{"sheets":[{"name":"指标","rows":[[{"formula":"1+2","format":"0.00"}]]}]}}"#).unwrap();
        let mut reordered: CreateArguments = serde_json::from_str(r#"{"format":"xlsx","title":"表格","workbook":{"sheets":[{"rows":[[{"format":"0.00","formula":"1+2"}]],"name":"指标"}]}}"#).unwrap();
        let destination = Some(Path::new("/workspace/output/report.xlsx"));
        let digest = creation_digest(&first.document, destination).unwrap();
        assert_eq!(
            digest,
            creation_digest(&reordered.document, destination).unwrap()
        );
        assert_ne!(
            digest,
            creation_digest(
                &reordered.document,
                Some(Path::new("/workspace/other.xlsx"))
            )
            .unwrap()
        );
        reordered.document.workbook.as_mut().unwrap()["sheets"][0]["rows"][0][0]["formula"] =
            json!("1+3");
        assert_ne!(
            digest,
            creation_digest(&reordered.document, destination).unwrap()
        );
    }
}
