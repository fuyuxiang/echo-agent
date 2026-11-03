//! One source shared by the document canvas, Agent tools, and local exports.
use crate::document_export::{DocumentExportReceipt, DocumentExportRequest};
use crate::document_store::{self, EditableDocument};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tauri::{AppHandle, Emitter, Manager};

type DocumentLock = tokio::sync::Mutex<()>;
static LOCKS: OnceLock<Mutex<HashMap<String, Weak<DocumentLock>>>> = OnceLock::new();
pub(crate) fn document_lock(path: &Path) -> Result<Arc<DocumentLock>, String> {
    let key = path.to_string_lossy().to_ascii_lowercase();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "文档保存锁已损坏")?;
    locks.retain(|_, value| value.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(DocumentLock::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}
fn file_hash(path: &Path) -> Result<(String, u64), String> {
    let bytes = crate::shell_fs::read_regular_file_bounded(path, 50 * 1024 * 1024)?;
    Ok((format!("{:x}", Sha256::digest(&bytes)), bytes.len() as u64))
}
fn resolve(app: &AppHandle, session: &str, path: &str) -> Result<PathBuf, String> {
    let cwd = app
        .state::<crate::commands::AppState>()
        .session_workspace(session)?;
    let access = app.state::<crate::shell_fs::FilesystemAccess>();
    let root = access.require_workspace(&cwd.to_string_lossy())?;
    let requested = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let canonical = access.require_authorized_file(&requested)?;
    if !canonical.starts_with(&root)
        && !crate::session_artifacts::has_export_path(session, &canonical)?
    {
        return Err("文档不属于当前任务的工作区或导出记录".into());
    }
    if !canonical
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "docx" | "md" | "markdown" | "mdown" | "mdx" | "txt"
            )
        })
    {
        return Err("此文件类型暂不支持文档编辑".into());
    }
    Ok(canonical)
}
fn load(session: &str, path: &Path) -> Result<Option<EditableDocument>, String> {
    let id = document_store::identity(session, path);
    let mut value = match document_store::read(&crate::paths::echo_agent_home_dir(), &id)? {
        Some(value) => value,
        None => return Ok(None),
    };
    if value.session_id != session || Path::new(&value.path) != path {
        return Err("文档归属不一致".into());
    }
    let (hash, size) = file_hash(path)?;
    if let Some((revision, pending_hash, pending_size)) = value.pending_export.take() {
        if hash == pending_hash {
            value.file_sha256 = pending_hash;
            value.byte_size = pending_size;
            value.exported_revision = revision;
        }
        document_store::write(&crate::paths::echo_agent_home_dir(), &value)?;
    }
    value.external_change = hash != value.file_sha256 || size != value.byte_size;
    Ok(Some(value))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDocument {
    pub document: Option<EditableDocument>,
    pub path: String,
    pub file_sha256: String,
    pub byte_size: u64,
}
fn notify(app: &AppHandle, document: &EditableDocument) {
    let _=app.emit("document-workspace-changed",serde_json::json!({"sessionId":document.session_id,"path":document.path,"revision":document.revision}));
}
pub(crate) fn register_generated(
    session: &str,
    path: &Path,
    title: &str,
    document: Value,
    sha256: &str,
    byte_size: u64,
) -> Result<EditableDocument, String> {
    document_store::validate(&document)?;
    let home = crate::paths::echo_agent_home_dir();
    let id = document_store::identity(session, path);
    let mut value = if let Some(mut previous) = document_store::read(&home, &id)? {
        let revision = previous.revision;
        document_store::revise(&mut previous, revision, document, "生成文档")?;
        previous
    } else {
        EditableDocument {
            id,
            session_id: session.into(),
            path: path.to_string_lossy().into_owned(),
            title: title.chars().take(160).collect(),
            revision: 1,
            exported_revision: 1,
            file_sha256: sha256.into(),
            byte_size,
            document,
            saved_at: document_store::now(),
            actor: "生成文档".into(),
            history: vec![],
            pending_export: None,
            external_change: false,
        }
    };
    value.file_sha256 = sha256.into();
    value.byte_size = byte_size;
    value.exported_revision = value.revision;
    value.external_change = false;
    document_store::write(&home, &value)?;
    Ok(value)
}
pub(crate) async fn open_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
) -> Result<OpenDocument, String> {
    let path = resolve(app, session, path)?;
    let lock = document_lock(&path)?;
    let _guard = lock.lock().await;
    let (file_sha256, byte_size) = file_hash(&path)?;
    Ok(OpenDocument {
        document: load(session, &path)?,
        path: path.to_string_lossy().into_owned(),
        file_sha256,
        byte_size,
    })
}
pub(crate) async fn save_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
    expected_revision: u64,
    document: Value,
    actor: &str,
) -> Result<EditableDocument, String> {
    let path = resolve(app, session, path)?;
    let lock = document_lock(&path)?;
    let _guard = lock.lock().await;
    let mut value = load(session, &path)?.ok_or("该文档尚未转换为可编辑稿")?;
    document_store::revise(&mut value, expected_revision, document, actor)?;
    document_store::write(&crate::paths::echo_agent_home_dir(), &value)?;
    notify(app, &value);
    Ok(value)
}
pub(crate) async fn publish_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
    expected_revision: u64,
) -> Result<EditableDocument, String> {
    let path = resolve(app, session, path)?;
    let lock = document_lock(&path)?;
    let _guard = lock.lock().await;
    let mut value = load(session, &path)?.ok_or("找不到文档编辑稿")?;
    if value.revision != expected_revision {
        return Err("DOCUMENT_CONFLICT:文档已有新修改，请保存最新内容后再更新 Word".into());
    }
    if value.external_change {
        return Err(
            "DOCUMENT_EXTERNAL_CHANGE:Word 文件已在其他位置修改，编辑稿已保留，请另存为副本".into(),
        );
    }
    if value.exported_revision == value.revision {
        return Ok(value);
    }
    let request = DocumentExportRequest {
        title: value.title.clone(),
        markdown: String::new(),
        format: "docx".into(),
        workbook: None,
        slides: None,
        document: Some(value.document.clone()),
    };
    let temp = tempfile::Builder::new()
        .prefix(".echo-document-")
        .tempdir_in(path.parent().ok_or("文件目录无效")?)
        .map_err(|e| e.to_string())?;
    let output = temp.path().join("revision.docx");
    crate::document_export::generate(app, &request, &output).await?;
    let (byte_size, sha256) = crate::document_export::validate_output(&output, "docx")?;
    if file_hash(&path)?.0 != value.file_sha256 {
        return Err("DOCUMENT_EXTERNAL_CHANGE:生成期间原文件发生变化，未覆盖，编辑稿已保留".into());
    }
    value.pending_export = Some((value.revision, sha256.clone(), byte_size));
    document_store::write(&crate::paths::echo_agent_home_dir(), &value)?;
    let receipt = DocumentExportReceipt {
        path: value.path.clone(),
        format: "docx".into(),
        byte_size,
        sha256: sha256.clone(),
    };
    crate::session_artifacts::stage(session, receipt.clone(), &value.title)?;
    crate::document_store::install_revision(&output, &path, &value.file_sha256)?;
    value.pending_export = None;
    value.file_sha256 = sha256;
    value.byte_size = byte_size;
    value.exported_revision = value.revision;
    document_store::write(&crate::paths::echo_agent_home_dir(), &value)?;
    crate::session_artifacts::record(session, receipt, &value.title)?;
    notify(app, &value);
    Ok(value)
}
pub(crate) async fn import_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
    expected_sha256: &str,
    document: Value,
) -> Result<EditableDocument, String> {
    document_store::validate(&document)?;
    let source = resolve(app, session, path)?;
    let lock = document_lock(&source)?;
    let _guard = lock.lock().await;
    if file_hash(&source)?.0 != expected_sha256 {
        return Err("DOCUMENT_EXTERNAL_CHANGE:原文件已变化，请重新读取后创建副本".into());
    }
    let cwd = app
        .state::<crate::commands::AppState>()
        .session_workspace(session)?;
    let root = app
        .state::<crate::shell_fs::FilesystemAccess>()
        .require_workspace(&cwd.to_string_lossy())?;
    let title = format!(
        "{}-可编辑副本",
        source.file_stem().unwrap_or_default().to_string_lossy()
    );
    let destination = crate::office_mcp::creation_destination(
        &root,
        &format!(
            "EchoAgent成果/{}-{}.docx",
            crate::document_export::safe_file_title(&title),
            uuid::Uuid::now_v7().simple()
        ),
        "docx",
    )?;
    let receipt = crate::document_export::create_for_session(
        app,
        DocumentExportRequest {
            title,
            markdown: String::new(),
            format: "docx".into(),
            workbook: None,
            slides: None,
            document: Some(document),
        },
        &destination,
        session,
        None,
    )
    .await?;
    let value = load(session, Path::new(&receipt.path))?
        .ok_or("已生成副本，但无法恢复编辑稿，请重新打开")?;
    notify(app, &value);
    Ok(value)
}
#[tauri::command]
pub async fn document_workspace_open(
    app: AppHandle,
    session_id: String,
    path: String,
) -> Result<OpenDocument, String> {
    open_for_session(&app, &session_id, &path).await
}
#[tauri::command]
pub async fn document_workspace_save(
    app: AppHandle,
    session_id: String,
    path: String,
    expected_revision: u64,
    document: Value,
) -> Result<EditableDocument, String> {
    save_for_session(
        &app,
        &session_id,
        &path,
        expected_revision,
        document,
        "手动编辑",
    )
    .await
}
#[tauri::command]
pub async fn document_workspace_publish(
    app: AppHandle,
    session_id: String,
    path: String,
    expected_revision: u64,
) -> Result<EditableDocument, String> {
    publish_for_session(&app, &session_id, &path, expected_revision).await
}
#[tauri::command]
pub async fn document_workspace_import(
    app: AppHandle,
    session_id: String,
    path: String,
    expected_sha256: String,
    document: Value,
) -> Result<EditableDocument, String> {
    import_for_session(&app, &session_id, &path, &expected_sha256, document).await
}
#[tauri::command]
pub async fn document_workspace_restore(
    app: AppHandle,
    session_id: String,
    path: String,
    expected_revision: u64,
    revision: u64,
) -> Result<EditableDocument, String> {
    let resolved = resolve(&app, &session_id, &path)?;
    let lock = document_lock(&resolved)?;
    let _guard = lock.lock().await;
    let mut value = load(&session_id, &resolved)?.ok_or("找不到编辑稿")?;
    let previous = value
        .history
        .iter()
        .find(|item| item.revision == revision)
        .ok_or("该历史版本已超出保留范围")?
        .document
        .clone();
    document_store::revise(&mut value, expected_revision, previous, "恢复历史")?;
    document_store::write(&crate::paths::echo_agent_home_dir(), &value)?;
    notify(&app, &value);
    Ok(value)
}
#[tauri::command]
pub async fn document_workspace_export(
    app: AppHandle,
    session_id: String,
    path: String,
    expected_revision: u64,
    format: String,
) -> Result<Option<DocumentExportReceipt>, String> {
    if !matches!(format.as_str(), "docx" | "pdf") {
        return Err("文档工作区支持导出 Word 或 PDF".into());
    }
    let resolved = resolve(&app, &session_id, &path)?;
    let value = {
        let lock = document_lock(&resolved)?;
        let _guard = lock.lock().await;
        let value = load(&session_id, &resolved)?.ok_or("找不到编辑稿")?;
        if value.revision != expected_revision {
            return Err("DOCUMENT_CONFLICT:文档已有新修改，请使用当前版本导出".into());
        }
        value
    };
    let receipt = crate::document_export::export_for_session(
        &app,
        DocumentExportRequest {
            title: value.title,
            markdown: String::new(),
            format,
            workbook: None,
            slides: None,
            document: Some(value.document),
        },
        Some(&session_id),
    )
    .await?;
    if let Some(receipt) = &receipt {
        app.state::<crate::shell_fs::FilesystemAccess>()
            .authorize_file(Path::new(&receipt.path))?;
    }
    Ok(receipt)
}
