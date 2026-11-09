//! One source shared by the document canvas, Agent tools, and local exports.
use crate::document_export::{DocumentExportReceipt, DocumentExportRequest};
use crate::document_store::{self, EditableDocument};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::HashMap,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tauri::{AppHandle, Emitter, Manager};

type DocumentLock = tokio::sync::Mutex<()>;
static LOCKS: OnceLock<Mutex<HashMap<String, Weak<DocumentLock>>>> = OnceLock::new();
pub(crate) fn document_lock(path: &Path) -> Result<Arc<DocumentLock>, String> {
    let key = dunce::simplified(path)
        .to_string_lossy()
        .to_ascii_lowercase();
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
fn session_root(app: &AppHandle, session: &str) -> Result<PathBuf, String> {
    let cwd = app
        .state::<crate::commands::AppState>()
        .session_workspace(session)?;
    let root = app
        .state::<crate::shell_fs::FilesystemAccess>()
        .require_workspace(&cwd.to_string_lossy())?;
    if dunce::simplified(&root) != dunce::simplified(&cwd) {
        return Err("原工作目录已被链接到其他位置，请恢复原目录".into());
    }
    Ok(root)
}

fn resolve(app: &AppHandle, session: &str, path: &str) -> Result<PathBuf, String> {
    let root = session_root(app, session)?;
    resolve_original(app, session, &root, &root.join(path))
}

fn require_document_extension(path: &Path) -> Result<(), String> {
    if path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "docx" | "md" | "markdown" | "mdown" | "mdx" | "txt"
        )
    }) {
        return Ok(());
    }
    Err("此文件类型暂不支持文档编辑".into())
}

fn resolve_original(
    app: &AppHandle,
    session: &str,
    root: &Path,
    requested: &Path,
) -> Result<PathBuf, String> {
    let canonical = app
        .state::<crate::shell_fs::FilesystemAccess>()
        .require_authorized_file(requested)?;
    if !canonical.starts_with(root)
        && !crate::session_artifacts::has_export_path(session, &canonical)?
    {
        return Err("文档不属于当前任务的工作区或导出记录".into());
    }
    require_document_extension(&canonical)?;
    Ok(canonical)
}

fn read_record_at(
    home: &Path,
    session: &str,
    path: &Path,
) -> Result<Option<EditableDocument>, String> {
    // Windows filesystem access and generated exports may spell the same
    // canonical path with or without the verbatim disk prefix.
    let simplified = dunce::simplified(path);
    let candidates = std::iter::once(path).chain((simplified != path).then_some(simplified));
    #[cfg(windows)]
    let verbatim = verbatim_disk_path(path);
    #[cfg(windows)]
    let candidates = candidates.chain(verbatim.as_deref());
    for candidate in candidates {
        let id = document_store::identity(session, candidate);
        if let Some(record) = document_store::read(home, &id)? {
            if record.session_id != session || Path::new(&record.path) != candidate {
                return Err("文档归属不一致".into());
            }
            return Ok(Some(record));
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn verbatim_disk_path(path: &Path) -> Option<PathBuf> {
    use std::path::Prefix;
    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    if !matches!(prefix.kind(), Prefix::Disk(_)) || !path.is_absolute() {
        return None;
    }
    let verbatim = PathBuf::from(format!(r"\\?\{}", path.to_str()?.replace('/', "\\")));
    (dunce::simplified(&verbatim) == path).then_some(verbatim)
}

pub(super) fn original_path_absent(error: &std::io::Error) -> bool {
    if error.kind() == ErrorKind::NotFound {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            ERROR_BAD_NETPATH, ERROR_BAD_NET_NAME, ERROR_DEVICE_NOT_CONNECTED, ERROR_DEV_NOT_EXIST,
            ERROR_INVALID_DRIVE, ERROR_NETWORK_UNREACHABLE, ERROR_NOT_READY,
        };
        matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(
                ERROR_BAD_NET_NAME
                    | ERROR_BAD_NETPATH
                    | ERROR_DEVICE_NOT_CONNECTED
                    | ERROR_DEV_NOT_EXIST
                    | ERROR_INVALID_DRIVE
                    | ERROR_NETWORK_UNREACHABLE
                    | ERROR_NOT_READY
            )
        )
    }
    #[cfg(not(windows))]
    false
}

/// A recoverable original must actually be missing. Permission errors, a
/// non-directory ancestor and a dangling link must not become a draft lookup.
fn canonical_missing_path(requested: &Path) -> Result<Option<PathBuf>, String> {
    if !requested.is_absolute()
        || requested
            .components()
            .any(|part| part == Component::ParentDir)
    {
        return Err("文档路径无效".into());
    }
    for ancestor in requested.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) if ancestor == requested => return Ok(None),
            Ok(_) => {
                let base = ancestor
                    .canonicalize()
                    .map_err(|error| format!("无法确认原文件目录：{error}"))?;
                if !base.is_dir() {
                    return Err("原文件的父路径不是目录".into());
                }
                let suffix = requested
                    .strip_prefix(ancestor)
                    .map_err(|_| "无法确认原文件路径")?;
                return Ok(Some(base.join(suffix)));
            }
            Err(error) if original_path_absent(&error) => {}
            Err(error) => return Err(format!("无法读取原文件状态：{error}")),
        }
    }
    // Windows can lose the entire drive, so there is no ancestor left to
    // canonicalize. Recovery still requires an exact existing draft and
    // trusted session ownership, and does not use this path for a file read.
    Ok(Some(requested.to_path_buf()))
}

fn recovery_scope_contains(
    home: &Path,
    session: &str,
    root: &Path,
    path: &Path,
) -> Result<bool, String> {
    let path = dunce::simplified(path);
    if path.starts_with(dunce::simplified(root)) {
        return Ok(true);
    }
    crate::session_artifacts::has_published_export_path_at(home, session, path)
}

pub(super) fn recovered_draft_at(
    home: &Path,
    session: &str,
    root: &Path,
    path: &Path,
) -> Result<Option<EditableDocument>, String> {
    require_document_extension(path)?;
    if !recovery_scope_contains(home, session, root, path)? {
        return Err("文档不属于当前任务的工作区或导出记录".into());
    }
    read_record_at(home, session, path)
}

fn reconcile_original(
    home: &Path,
    mut value: EditableDocument,
    hash: &str,
    size: u64,
) -> Result<EditableDocument, String> {
    if let Some((revision, pending_hash, pending_size)) = value.pending_export.take() {
        if hash == pending_hash {
            value.file_sha256 = pending_hash;
            value.byte_size = pending_size;
            value.exported_revision = revision;
        }
        document_store::write(home, &value)?;
    }
    value.external_change = hash != value.file_sha256 || size != value.byte_size;
    Ok(value)
}

fn load(session: &str, path: &Path) -> Result<Option<EditableDocument>, String> {
    let home = crate::paths::echo_agent_home_dir();
    let Some(value) = read_record_at(&home, session, path)? else {
        return Ok(None);
    };
    let (hash, size) = file_hash(path)?;
    reconcile_original(&home, value, &hash, size).map(Some)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDocument {
    pub document: Option<EditableDocument>,
    pub path: String,
    pub file_sha256: String,
    pub byte_size: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub source_missing: bool,
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
    let parent = path.parent().ok_or("无效的文档目录")?;
    let name = path.file_name().ok_or("无效的文档文件名")?;
    let path = parent
        .canonicalize()
        .map_err(|error| format!("无法确认已生成文档的目录：{error}"))?
        .join(name);
    let mut value = if let Some(mut previous) = read_record_at(&home, session, &path)? {
        let revision = previous.revision;
        document_store::revise(&mut previous, revision, document, "生成文档")?;
        previous
    } else {
        let path = dunce::simplified(&path);
        EditableDocument {
            id: document_store::identity(session, path),
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentOpenMode {
    OriginalOnly,
    AllowRecovery,
    MissingOnly,
}

async fn open_document(
    app: &AppHandle,
    session: &str,
    path: &str,
    mode: DocumentOpenMode,
) -> Result<OpenDocument, String> {
    let root = session_root(app, session);
    let home = crate::paths::echo_agent_home_dir();
    if mode != DocumentOpenMode::OriginalOnly {
        // Historical ownership permits reading only the durable draft. A
        // closed or offline workspace never acquires live file permissions.
        let history_root = match &root {
            Ok(root) => Cow::Borrowed(root.as_path()),
            Err(_) => Cow::Owned(crate::commands::trusted_session_history_cwd(
                &app.state::<crate::commands::AppState>(),
                session,
                None,
            )?),
        };
        let requested = history_root.join(path);
        let missing = if recovery_scope_contains(&home, session, &history_root, &requested)? {
            canonical_missing_path(&requested)?
        } else {
            None
        };
        if let Some(missing) = missing {
            let lock = document_lock(&missing)?;
            let _guard = lock.lock().await;
            // Recheck after acquiring the lock. A restored/recreated original
            // follows the ordinary file authorization path.
            if canonical_missing_path(&requested)?.as_ref() == Some(&missing) {
                let record = recovered_draft_at(&home, session, &history_root, &missing)?;
                if mode == DocumentOpenMode::MissingOnly && record.is_none() {
                    return Err("原文件不存在，也没有可恢复的编辑稿".into());
                }
                return Ok(OpenDocument {
                    path: record
                        .as_ref()
                        .map(|record| record.path.clone())
                        .unwrap_or_else(|| missing.to_string_lossy().into_owned()),
                    document: record,
                    file_sha256: String::new(),
                    byte_size: 0,
                    source_missing: true,
                });
            }
        }
    }
    if mode == DocumentOpenMode::MissingOnly {
        return Err("没有可打开的缺失原件编辑稿".into());
    }
    let root = root?;
    let path = resolve_original(app, session, &root, &root.join(path))?;
    let lock = document_lock(&path)?;
    let _guard = lock.lock().await;
    let (file_sha256, byte_size) = file_hash(&path)?;
    let document = read_record_at(&home, session, &path)?
        .map(|value| reconcile_original(&home, value, &file_sha256, byte_size))
        .transpose()?;
    Ok(OpenDocument {
        document,
        path: path.to_string_lossy().into_owned(),
        file_sha256,
        byte_size,
        source_missing: false,
    })
}

pub(crate) async fn open_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
) -> Result<OpenDocument, String> {
    open_document(app, session, path, DocumentOpenMode::OriginalOnly).await
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
        path: dunce::simplified(&path).to_string_lossy().into_owned(),
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
fn matches_source_preview(path: &Path, preview: &str) -> Result<bool, String> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("docx"))
    {
        let encoded = preview.strip_prefix("data:application/vnd.openxmlformats-officedocument.wordprocessingml.document;base64,")
            .unwrap_or(preview);
        use crate::shell_fs::binary_preview_limits::OOXML_PREVIEW_MAX_BYTES;
        if encoded.len() > OOXML_PREVIEW_MAX_BYTES.div_ceil(3) * 4 {
            return Ok(false);
        }
        let bytes = STANDARD.decode(encoded).map_err(|_| "预览文件编码无效")?;
        let current =
            crate::shell_fs::read_regular_file_bounded(path, OOXML_PREVIEW_MAX_BYTES as u64)
                .map_err(|error| format!("DOCUMENT_EXTERNAL_CHANGE:无法读取完整原文件：{error}"))?;
        Ok(bytes == current)
    } else {
        const PREVIEW_LIMIT: usize = 256 * 1024;
        if preview.len() > PREVIEW_LIMIT {
            return Ok(false);
        }
        let current = crate::shell_fs::read_regular_file_bounded(path, (PREVIEW_LIMIT + 1) as u64)
            .map_err(|error| format!("DOCUMENT_EXTERNAL_CHANGE:无法读取完整原文件：{error}"))?;
        Ok(current == preview.as_bytes())
    }
}

async fn import_for_session(
    app: &AppHandle,
    session: &str,
    path: &str,
    expected_sha256: Option<&str>,
    source_preview: Option<&str>,
    document: Value,
) -> Result<EditableDocument, String> {
    document_store::validate(&document)?;
    let source = resolve(app, session, path)?;
    let lock = document_lock(&source)?;
    let _guard = lock.lock().await;
    let matches = match (source_preview, expected_sha256) {
        (Some(preview), None) => matches_source_preview(&source, preview)?,
        (None, Some(sha256)) => {
            read_record_at(&crate::paths::echo_agent_home_dir(), session, &source)?.is_some()
                && file_hash(&source)?.0 == sha256
        }
        _ => return Err("请重新读取原件后创建副本".into()),
    };
    if !matches {
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
    let destination = resolve_original(app, session, &root, Path::new(&receipt.path))?;
    let value = load(session, &destination)?.ok_or("已生成副本，但无法恢复编辑稿，请重新打开")?;
    notify(app, &value);
    Ok(value)
}
#[tauri::command]
pub async fn document_workspace_open(
    app: AppHandle,
    session_id: String,
    path: String,
    recover_missing_only: Option<bool>,
) -> Result<OpenDocument, String> {
    let mode = if recover_missing_only == Some(true) {
        DocumentOpenMode::MissingOnly
    } else {
        DocumentOpenMode::AllowRecovery
    };
    open_document(&app, &session_id, &path, mode).await
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
    expected_sha256: Option<String>,
    source_preview: Option<String>,
    document: Value,
) -> Result<EditableDocument, String> {
    import_for_session(
        &app,
        &session_id,
        &path,
        expected_sha256.as_deref(),
        source_preview.as_deref(),
        document,
    )
    .await
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
    recovered_copy: Option<bool>,
) -> Result<Option<DocumentExportReceipt>, String> {
    if !matches!(format.as_str(), "docx" | "pdf") {
        return Err("文档工作区支持导出 Word 或 PDF".into());
    }
    let opened = open_document(&app, &session_id, &path, DocumentOpenMode::AllowRecovery).await?;
    let value = opened.document.ok_or("找不到编辑稿")?;
    if value.revision != expected_revision {
        return Err("DOCUMENT_CONFLICT:文档已有新修改，请使用当前版本导出".into());
    }
    let request = DocumentExportRequest {
        title: value.title,
        markdown: String::new(),
        format,
        workbook: None,
        slides: None,
        document: Some(value.document),
    };
    let receipt = if opened.source_missing || recovered_copy == Some(true) {
        crate::document_export::export_recovered_for_session(
            &app,
            request,
            &session_id,
            Path::new(&opened.path),
        )
        .await?
    } else {
        crate::document_export::export_for_session(&app, request, Some(&session_id)).await?
    };
    if let Some(receipt) = &receipt {
        app.state::<crate::shell_fs::FilesystemAccess>()
            .authorize_file(Path::new(&receipt.path))?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod source_preview_tests {
    use super::*;

    #[test]
    fn recovered_draft_uses_the_same_session_and_canonical_missing_path() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let home = root.join("profile");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = workspace.join("成果/缺失子目录/原件.docx");
        let document = serde_json::json!({
            "type": "doc",
            "content": [{"type": "paragraph", "content": [{"type": "text", "text": "已保存的恢复稿"}]}],
        });
        let record = EditableDocument {
            id: document_store::identity("owner", &path),
            session_id: "owner".into(),
            path: path.to_string_lossy().into_owned(),
            title: "报告".into(),
            revision: 3,
            exported_revision: 1,
            file_sha256: "a".repeat(64),
            byte_size: 12,
            document: document.clone(),
            saved_at: 10,
            actor: "手动编辑".into(),
            history: vec![document_store::DocumentVersion {
                revision: 2,
                saved_at: 9,
                actor: "手动编辑".into(),
                document: document.clone(),
            }],
            pending_export: None,
            external_change: false,
        };
        document_store::write(&home, &record).unwrap();
        let missing = canonical_missing_path(&path).unwrap().unwrap();
        let recovered = recovered_draft_at(&home, "owner", &workspace, &missing)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.document, document);
        assert_eq!((recovered.revision, recovered.exported_revision), (3, 1));
        assert_eq!(recovered.history[0].revision, 2);
        assert!(
            recovered_draft_at(&home, "another-session", &workspace, &missing)
                .unwrap()
                .is_none()
        );
        assert!(
            recovered_draft_at(&home, "owner", &workspace, &root.join("foreign.docx")).is_err()
        );
        assert!(!path.parent().unwrap().exists());

        std::fs::remove_dir(&workspace).unwrap();
        let missing = canonical_missing_path(&path).unwrap().unwrap();
        let recovered_offline = recovered_draft_at(&home, "owner", &workspace, &missing)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_offline.id, recovered.id);
        assert!(!workspace.exists());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"existing").unwrap();
        assert_eq!(canonical_missing_path(&path).unwrap(), None);
        assert!(canonical_missing_path(&workspace.join("../foreign.docx")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn canonical_windows_path_reopens_legacy_export_and_preserves_history() {
        let home = tempfile::tempdir().unwrap();
        let path = Path::new(r"C:\workspace\report.docx");
        let canonical = Path::new(r"\\?\C:\workspace\report.docx");
        let document = serde_json::json!({"type":"doc","content":[{"type":"paragraph"}]});
        let legacy = EditableDocument {
            id: document_store::identity("owner", path),
            session_id: "owner".into(),
            path: path.to_string_lossy().into_owned(),
            title: "报告".into(),
            revision: 3,
            exported_revision: 2,
            file_sha256: "a".repeat(64),
            byte_size: 12,
            document: document.clone(),
            saved_at: 10,
            actor: "手动编辑".into(),
            history: vec![document_store::DocumentVersion {
                revision: 2,
                saved_at: 9,
                actor: "手动编辑".into(),
                document,
            }],
            pending_export: None,
            external_change: false,
        };
        document_store::write(home.path(), &legacy).unwrap();
        let reopened = read_record_at(home.path(), "owner", canonical)
            .unwrap()
            .unwrap();
        assert_eq!(reopened.id, legacy.id);
        assert_eq!(reopened.revision, 3);
        assert_eq!(reopened.history[0].revision, 2);

        let mut verbatim = legacy;
        verbatim.session_id = "other-owner".into();
        verbatim.path = canonical.to_string_lossy().into_owned();
        verbatim.id = document_store::identity(&verbatim.session_id, canonical);
        document_store::write(home.path(), &verbatim).unwrap();
        assert_eq!(
            read_record_at(home.path(), "other-owner", path)
                .unwrap()
                .unwrap()
                .id,
            verbatim.id
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unmounted_drive_keeps_the_exact_path_for_draft_lookup() {
        let missing_drive = ('D'..='Z')
            .map(|letter| PathBuf::from(format!(r"{letter}:\")))
            .find(|path| {
                std::fs::symlink_metadata(path).is_err_and(|error| original_path_absent(&error))
            });
        if let Some(drive) = missing_drive {
            let requested = drive.join("work/report.docx");
            assert_eq!(canonical_missing_path(&requested).unwrap(), Some(requested));
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_links_do_not_count_as_missing_originals() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link.docx");
        std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
        assert_eq!(canonical_missing_path(&link).unwrap(), None);
        assert!(canonical_missing_path(&link.join("source.docx")).is_err());
    }

    #[test]
    fn editable_copy_requires_complete_unchanged_source_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let markdown = dir.path().join("源稿.md");
        std::fs::write(&markdown, "100 元").unwrap();
        assert!(matches_source_preview(&markdown, "100 元").unwrap());
        std::fs::write(&markdown, "900 元").unwrap();
        assert!(!matches_source_preview(&markdown, "100 元").unwrap());

        let prefix = "a".repeat(256 * 1024);
        std::fs::write(&markdown, format!("{prefix}尾")).unwrap();
        assert!(!matches_source_preview(
            &markdown,
            &format!("{prefix}\n\n…(已截断，仅预览前部分内容)")
        )
        .unwrap());
        assert!(matches_source_preview(&markdown, &prefix).is_err());

        let word = dir.path().join("源稿.docx");
        let encoded = STANDARD.encode([1, 2, 3]);
        std::fs::write(&word, [1, 2, 3]).unwrap();
        assert!(matches_source_preview(&word, &encoded).unwrap());
        std::fs::write(&word, [1, 2, 4]).unwrap();
        assert!(!matches_source_preview(&word, &encoded).unwrap());
    }
}
