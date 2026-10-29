//! The same versioned document protocol for a project whose saved IDE has been
//! unmounted. Authority remains the pending Runtime tool and its live run.
use crate::{
    coding_document_bridge,
    coding_workspace::{self, CodingWriteDocumentRequest},
    shell_fs::FilesystemAccess,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tauri::State;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_PREFLIGHT_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    line: u32,
    character: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    start: Position,
    end: Position,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    range: Range,
    text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTarget {
    uri: String,
    expected_version: Option<u64>,
    expected_hash: String,
    edits: Vec<TextEdit>,
    save: Option<bool>,
    #[serde(default)]
    delete: bool,
    #[serde(default)]
    move_to: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOperation {
    operation_id: String,
    session_id: String,
    run_id: String,
    workspace: String,
    files: Vec<EditTarget>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSnapshot {
    workspace: String,
    uri: String,
    path: String,
    language: String,
    version: Option<u64>,
    hash: String,
    dirty: bool,
    source: &'static str,
    exists: bool,
    selection: Option<Range>,
    selected_text: String,
    content: String,
    content_offset: usize,
    total_characters: usize,
    truncated: bool,
    captured_at: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FileStatus {
    Applied,
    Conflict,
    Rejected,
    Failed,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileApplyResult {
    uri: String,
    status: FileStatus,
    version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    save_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    moved_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    moved_hash: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    operation_id: String,
    session_id: String,
    run_id: String,
    status: &'static str,
    files: Vec<FileApplyResult>,
}
struct CachedResult {
    fingerprint: String,
    result: ApplyResult,
}
static RESULTS: OnceLock<Mutex<HashMap<String, CachedResult>>> = OnceLock::new();

fn error_result(uri: &str, status: FileStatus, error: impl Into<String>) -> FileApplyResult {
    FileApplyResult {
        uri: uri.into(),
        status,
        version: None,
        hash: None,
        dirty: None,
        error: Some(error.into()),
        save_error: None,
        moved_to: None,
        moved_hash: None,
    }
}

fn decode_path(claimed: &str) -> Result<PathBuf, String> {
    if claimed.starts_with("file:") {
        return url::Url::parse(claimed)
            .map_err(|_| "文档 URI 无效".to_string())?
            .to_file_path()
            .map_err(|_| "只支持本地文件 URI".to_string());
    }
    if claimed.contains("://") {
        return Err("只支持工作区内的本地文件".into());
    }
    Ok(PathBuf::from(claimed))
}

/// Resolve missing ancestors without creating anything during snapshot/preflight.
pub(crate) fn target_path(root: &Path, claimed: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("无法解析工作区：{error}"))?;
    let root = canonical_root.as_path();
    let raw = decode_path(claimed)?;
    if raw.components().any(|component| {
        matches!(component, Component::ParentDir)
            || component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
    }) {
        return Err("拒绝目录跳转和 Git 内部文件".into());
    }
    let candidate = if raw.is_absolute() {
        raw
    } else {
        root.join(raw)
    };
    let name = candidate
        .file_name()
        .ok_or("文档缺少文件名")?
        .to_os_string();
    let mut parent = candidate.parent().ok_or("文档缺少父目录")?.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err("文档父路径不是普通目录".into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    parent
                        .file_name()
                        .ok_or("无法解析文档父路径")?
                        .to_os_string(),
                );
                parent = parent.parent().ok_or("无法解析文档父路径")?.to_path_buf();
            }
            Err(error) => return Err(format!("无法读取文档父目录：{error}")),
        }
    }
    let mut canonical = parent
        .canonicalize()
        .map_err(|error| format!("无法解析文档父目录：{error}"))?;
    if !canonical.starts_with(root) {
        return Err("拒绝访问工作区之外的文档".into());
    }
    for part in missing.iter().rev() {
        canonical.push(part);
    }
    canonical.push(name);
    Ok(canonical)
}

fn snapshot(root: &Path, workspace: &str, claimed: &str) -> Result<DocumentSnapshot, String> {
    let path = target_path(root, claimed)?;
    let (content, hash, language, exists, path) = match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let document = coding_workspace::read_coding_document(root, &path.to_string_lossy())?;
            (
                document.content,
                document.hash,
                document.language,
                true,
                PathBuf::from(document.path),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            String::new(),
            "missing".into(),
            coding_workspace::editor_language(&path),
            false,
            path,
        ),
        Err(error) => return Err(format!("无法读取文档：{error}")),
    };
    Ok(DocumentSnapshot {
        workspace: workspace.into(),
        uri: url::Url::from_file_path(&path)
            .map_err(|_| "无法创建文档 URI")?
            .to_string(),
        path: path.to_string_lossy().into(),
        language,
        version: None,
        hash,
        dirty: false,
        source: "disk",
        exists,
        selection: None,
        selected_text: String::new(),
        content_offset: 0,
        total_characters: content.encode_utf16().count(),
        content,
        truncated: false,
        captured_at: chrono::Utc::now().to_rfc3339(),
    })
}

fn position_offset(text: &str, position: &Position) -> Result<usize, String> {
    let mut start = 0;
    for _ in 0..position.line {
        start += text[start..].find('\n').ok_or("编辑行号超出文档")? + 1;
    }
    let end = text[start..]
        .find('\n')
        .map(|index| start + index)
        .unwrap_or(text.len());
    let line = text[start..end]
        .strip_suffix('\r')
        .unwrap_or(&text[start..end]);
    let mut units = 0u32;
    for (byte, character) in line.char_indices() {
        if units == position.character {
            return Ok(start + byte);
        }
        units += character.len_utf16() as u32;
        if units > position.character {
            return Err("编辑位置不能切开 Unicode 代理对".into());
        }
    }
    if units == position.character {
        Ok(start + line.len())
    } else {
        Err("编辑列号超出文档".into())
    }
}

fn edited_text(content: &str, edits: &[TextEdit]) -> Result<String, String> {
    if content.len() > MAX_BYTES || content.contains('\0') || edits.len() > 2000 {
        return Err("文档类型或大小不支持文本编辑".into());
    }
    let mut spans = Vec::with_capacity(edits.len());
    let mut inserted = 0usize;
    for edit in edits {
        inserted = inserted.saturating_add(edit.text.len());
        if inserted > MAX_BYTES || edit.text.contains('\0') {
            return Err("文本编辑超过预算或包含二进制数据".into());
        }
        let start = position_offset(content, &edit.range.start)?;
        let end = position_offset(content, &edit.range.end)?;
        if start > end {
            return Err("编辑范围起点晚于终点".into());
        }
        spans.push((start, end, &edit.text));
    }
    spans.sort_by_key(|span| (span.0, span.1));
    for pair in spans.windows(2) {
        if pair[1].0 < pair[0].1 || pair[1].0 == pair[0].0 {
            return Err("同一操作中的编辑范围重叠".into());
        }
    }
    let mut result = content.to_string();
    for (start, end, text) in spans.into_iter().rev() {
        result.replace_range(start..end, text);
    }
    if result.len() > MAX_BYTES {
        return Err("应用后的文档超过 2 MB 安全上限".into());
    }
    Ok(result)
}

struct Prepared {
    target: EditTarget,
    path: PathBuf,
    text: String,
    missing: bool,
}
enum Preflight {
    Ready(Prepared),
    Error(FileApplyResult),
}

fn apply(root: &Path, operation: EditOperation) -> Result<ApplyResult, String> {
    if operation.files.is_empty()
        || operation.files.len() > 50
        || [
            &operation.operation_id,
            &operation.session_id,
            &operation.run_id,
        ]
        .iter()
        .any(|id| id.is_empty() || id.len() > 256)
    {
        return Err("修改缺少身份或文件数量超过预算".into());
    }
    let serialized = serde_json::to_vec(&operation).map_err(|error| error.to_string())?;
    if serialized.len() > 3 * MAX_BYTES / 2 {
        return Err("修改请求超过预算".into());
    }
    let key = serde_json::to_string(&(
        &operation.workspace,
        &operation.session_id,
        &operation.run_id,
        &operation.operation_id,
    ))
    .map_err(|error| error.to_string())?;
    let fingerprint = coding_workspace::document_hash(&serialized);
    let mut results = RESULTS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "文档幂等状态已损坏")?;
    if let Some(cached) = results.get(&key) {
        if cached.fingerprint != fingerprint {
            return Err("operationId 已用于另一组修改".into());
        }
        return Ok(cached.result.clone());
    }
    let mut preflight = Vec::new();
    let mut seen = HashSet::new();
    let mut consumed = 0usize;
    for target in &operation.files {
        let prepared = (|| -> Result<Prepared, FileApplyResult> {
            let path = target_path(root, &target.uri)
                .map_err(|error| error_result(&target.uri, FileStatus::Rejected, error))?;
            if !seen.insert(path.clone()) {
                return Err(error_result(
                    &target.uri,
                    FileStatus::Rejected,
                    "同一操作重复包含文档",
                ));
            }
            let current = snapshot(root, &operation.workspace, &target.uri)
                .map_err(|error| error_result(&target.uri, FileStatus::Rejected, error))?;
            consumed = consumed.saturating_add(current.content.len());
            if consumed > MAX_PREFLIGHT_BYTES {
                return Err(error_result(
                    &target.uri,
                    FileStatus::Rejected,
                    "多文件预检超过大小预算",
                ));
            }
            if current.hash != target.expected_hash {
                return Err(error_result(
                    &target.uri,
                    FileStatus::Conflict,
                    "文档内容已变化，未覆盖原内容",
                ));
            }
            if target.move_to.as_ref().is_some_and(|destination| {
                target_path(root, destination)
                    .map(|path| path.exists())
                    .unwrap_or(true)
            }) || ((target.delete || target.move_to.is_some()) && !current.exists)
            {
                return Err(error_result(
                    &target.uri,
                    FileStatus::Conflict,
                    "移动目标已存在或源文件不存在",
                ));
            }
            if current.exists
                && std::fs::metadata(&path)
                    .map(|metadata| metadata.permissions().readonly())
                    .unwrap_or(true)
            {
                return Err(error_result(
                    &target.uri,
                    FileStatus::Rejected,
                    "文档为只读，未应用修改",
                ));
            }
            let text = edited_text(&current.content, &target.edits)
                .map_err(|error| error_result(&target.uri, FileStatus::Rejected, error))?;
            // An unmounted, saved document has no Monaco version. Its complete hash is authoritative.
            Ok(Prepared {
                target: target.clone(),
                path,
                text,
                missing: !current.exists,
            })
        })();
        preflight.push(match prepared {
            Ok(prepared) => Preflight::Ready(prepared),
            Err(error) => Preflight::Error(error),
        });
    }
    let mut files = Vec::new();
    for prepared in preflight {
        let prepared = match prepared {
            Preflight::Ready(prepared) => prepared,
            Preflight::Error(error) => {
                files.push(error);
                continue;
            }
        };
        let write = if prepared.target.delete {
            remove_or_move(root, &prepared.path, &prepared.target.expected_hash, None)
        } else if prepared.missing {
            create_new(root, &prepared)
        } else {
            coding_workspace::write_coding_document_blocking(
                root,
                CodingWriteDocumentRequest {
                    root: operation.workspace.clone(),
                    path: prepared.path.to_string_lossy().into(),
                    content: prepared.text.clone(),
                    expected_hash: prepared.target.expected_hash.clone(),
                },
            )
            .map(|_| ())
        };
        match write {
            Ok(()) => {
                let hash = coding_workspace::document_hash(prepared.text.as_bytes());
                let moved = prepared
                    .target
                    .move_to
                    .as_deref()
                    .map(|destination| {
                        let destination = target_path(root, destination)?;
                        remove_or_move(root, &prepared.path, &hash, Some(&destination))?;
                        url::Url::from_file_path(destination)
                            .map(|uri| uri.to_string())
                            .map_err(|_| "移动目标 URI 无效".to_string())
                    })
                    .transpose();
                let (moved_to, save_error) = match moved {
                    Ok(value) => (value, None),
                    Err(error) => (None, Some(format!("内容已应用，但移动失败：{error}"))),
                };
                let removed = prepared.target.delete || moved_to.is_some();
                files.push(FileApplyResult {
                    uri: prepared.target.uri,
                    status: FileStatus::Applied,
                    version: None,
                    hash: Some(if removed {
                        "missing".into()
                    } else {
                        hash.clone()
                    }),
                    dirty: Some(false),
                    error: None,
                    save_error,
                    moved_hash: moved_to.as_ref().map(|_| hash),
                    moved_to,
                });
            }
            Err(error) => files.push(error_result(
                &prepared.target.uri,
                if error.contains("冲突") || error.contains("已存在") {
                    FileStatus::Conflict
                } else {
                    FileStatus::Failed
                },
                error,
            )),
        }
    }
    let applied = files
        .iter()
        .filter(|file| file.status == FileStatus::Applied)
        .count();
    let status = if applied == files.len() {
        "applied"
    } else if applied > 0 {
        "partial"
    } else if files.iter().any(|file| file.status == FileStatus::Conflict) {
        "conflict"
    } else if files.iter().any(|file| file.status == FileStatus::Failed) {
        "failed"
    } else {
        "rejected"
    };
    let result = ApplyResult {
        operation_id: operation.operation_id,
        session_id: operation.session_id,
        run_id: operation.run_id,
        status,
        files,
    };
    results.insert(
        key,
        CachedResult {
            fingerprint,
            result: result.clone(),
        },
    );
    Ok(result)
}

fn remove_or_move(
    root: &Path,
    source: &Path,
    expected: &str,
    destination: Option<&Path>,
) -> Result<(), String> {
    let transaction = coding_workspace::document_write_lock(source)?;
    let _guard = transaction.lock().map_err(|_| "文档写入协调状态已损坏")?;
    let parent = source.parent().ok_or("源文件目录无效")?;
    let temporary = parent.join(format!(".echo-move-{}", uuid::Uuid::now_v7()));
    // Rename first, then compare the actual retained inode before removing it.
    // Concurrently replaced content is restored or retained, never unlinked.
    std::fs::rename(source, &temporary).map_err(|error| error.to_string())?;
    let result = (|| {
        let document = coding_workspace::read_coding_document(root, &temporary.to_string_lossy())?;
        if document.hash != expected {
            return Err("文件冲突，未删除或移动外部修改".into());
        }
        if let Some(destination) = destination {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            if target_path(root, &destination.to_string_lossy())? != destination {
                return Err("移动目标目录发生变化".into());
            }
            std::fs::hard_link(&temporary, destination)
                .map_err(|error| format!("移动目标已存在或不可用：{error}"))?;
        }
        std::fs::remove_file(&temporary).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        // Exclusive hard link cannot replace a newly-created source file.
        if std::fs::hard_link(&temporary, source).is_ok() {
            let _ = std::fs::remove_file(&temporary);
        }
    }
    result
}

fn create_new(root: &Path, prepared: &Prepared) -> Result<(), String> {
    let transaction = coding_workspace::document_write_lock(&prepared.path)?;
    let _guard = transaction.lock().map_err(|_| "文档写入协调状态已损坏")?;
    let confirmed = target_path(root, &prepared.target.uri)?;
    if confirmed != prepared.path || std::fs::symlink_metadata(&confirmed).is_ok() {
        return Err("新建冲突：目标已存在或父目录已变化".into());
    }
    let parent = confirmed.parent().ok_or("新文档缺少父目录")?;
    std::fs::create_dir_all(parent).map_err(|error| format!("创建文档目录失败：{error}"))?;
    if target_path(root, &prepared.target.uri)? != confirmed {
        return Err("新建冲突：父目录已变化".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&confirmed)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "新建冲突：目标已存在".into()
            } else {
                format!("创建文档失败：{error}")
            }
        })?;
    file.write_all(prepared.text.as_bytes())
        .map_err(|error| format!("新文档写入失败：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("新文档同步失败：{error}"))
}

#[tauri::command]
pub async fn coding_closed_document_snapshot(
    access: State<'_, FilesystemAccess>,
    workspace: String,
    session_id: String,
    run_id: String,
    operation_id: String,
    path: String,
) -> Result<DocumentSnapshot, String> {
    coding_document_bridge::validate_pending_target(
        &operation_id,
        &workspace,
        &session_id,
        &run_id,
        &path,
        false,
    )
    .await?;
    let root = access.require_workspace(&workspace)?;
    if coding_document_bridge::has_editor(&root.to_string_lossy()).await {
        return Err("工程编辑器已连接，必须读取实际编辑缓冲".into());
    }
    let claimed = workspace.clone();
    let result = tokio::task::spawn_blocking(move || snapshot(&root, &claimed, &path))
        .await
        .map_err(|error| error.to_string())??;
    coding_document_bridge::validate_pending(&operation_id, &workspace, &session_id, &run_id)
        .await?;
    Ok(result)
}

#[tauri::command]
pub async fn coding_closed_document_apply(
    access: State<'_, FilesystemAccess>,
    operation: EditOperation,
) -> Result<ApplyResult, String> {
    if operation.files.is_empty() || operation.files.len() > 50 {
        return Err("文档操作需要 1～50 个文件".into());
    }
    for file in &operation.files {
        for path in std::iter::once(file.uri.as_str()).chain(file.move_to.as_deref()) {
            coding_document_bridge::validate_pending_target(
                &operation.operation_id,
                &operation.workspace,
                &operation.session_id,
                &operation.run_id,
                path,
                true,
            )
            .await?;
        }
    }
    let root = access.require_workspace(&operation.workspace)?;
    coding_document_bridge::with_pending_action(
        operation.operation_id.clone(),
        operation.workspace.clone(),
        operation.session_id.clone(),
        operation.run_id.clone(),
        move || apply(&root, operation),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation(root: &Path, files: Vec<EditTarget>) -> EditOperation {
        EditOperation {
            operation_id: uuid::Uuid::now_v7().to_string(),
            session_id: "closed-tests".into(),
            run_id: "run".into(),
            workspace: root.to_string_lossy().into(),
            files,
        }
    }
    fn target(root: &Path, name: &str, old: &str, new: &str) -> EditTarget {
        EditTarget {
            uri: url::Url::from_file_path(root.join(name))
                .unwrap()
                .to_string(),
            expected_version: None,
            expected_hash: coding_workspace::document_hash(old.as_bytes()),
            save: Some(true),
            delete: false,
            move_to: None,
            edits: vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: old.encode_utf16().count() as u32,
                    },
                },
                text: new.into(),
            }],
        }
    }

    #[test]
    fn closed_snapshot_is_complete_utf16_and_missing_has_no_side_effect() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let text = "😀".repeat(40_000);
        std::fs::write(root.join("a.ts"), &text).unwrap();
        let document = snapshot(&root, "native-workspace-spelling", "a.ts").unwrap();
        assert_eq!(document.content, text);
        assert_eq!(document.total_characters, 80_000);
        assert_eq!(document.workspace, "native-workspace-spelling");
        assert_eq!(document.source, "disk");
        assert!(!document.dirty);
        assert!(!document.truncated);
        let missing = snapshot(&root, "workspace", "nested/new.ts").unwrap();
        assert_eq!(missing.hash, "missing");
        assert!(!missing.exists);
        assert!(!root.join("nested").exists());
    }

    #[test]
    fn closed_multi_file_conflicts_are_partial_idempotent_and_preserve_modes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ts"), "old").unwrap();
        std::fs::write(root.join("b.ts"), "user").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("a.ts"), std::fs::Permissions::from_mode(0o751))
                .unwrap();
        }
        let request = operation(
            &root,
            vec![
                target(&root, "a.ts", "old", "new"),
                target(&root, "b.ts", "stale", "agent"),
            ],
        );
        let result = apply(&root, request.clone()).unwrap();
        assert_eq!(result.status, "partial");
        assert_eq!(result.files[0].status, FileStatus::Applied);
        assert_eq!(result.files[1].status, FileStatus::Conflict);
        assert_eq!(std::fs::read_to_string(root.join("a.ts")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(root.join("b.ts")).unwrap(), "user");
        std::fs::write(root.join("a.ts"), "subsequent user change").unwrap();
        assert_eq!(apply(&root, request.clone()).unwrap().status, "partial");
        assert_eq!(
            std::fs::read_to_string(root.join("a.ts")).unwrap(),
            "subsequent user change"
        );
        let mut reused = request;
        reused.files[0].edits[0].text = "different".into();
        assert!(apply(&root, reused).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("a.ts"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
        }
    }

    #[test]
    fn closed_utf16_edits_reject_surrogate_overlap_and_unsafe_paths() {
        let text = "😀abc\r\nsecond";
        assert_eq!(
            position_offset(
                text,
                &Position {
                    line: 0,
                    character: 2
                }
            )
            .unwrap(),
            4
        );
        assert!(position_offset(
            text,
            &Position {
                line: 0,
                character: 1
            }
        )
        .is_err());
        let duplicate = TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 2,
                },
                end: Position {
                    line: 0,
                    character: 3,
                },
            },
            text: "x".into(),
        };
        assert!(edited_text(text, &[duplicate.clone(), duplicate]).is_err());
        let edits = vec![
            TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 2,
                    },
                    end: Position {
                        line: 0,
                        character: 5,
                    },
                },
                text: "new".into(),
            },
            TextEdit {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 6,
                    },
                },
                text: "tail".into(),
            },
        ];
        assert_eq!(edited_text(text, &edits).unwrap(), "😀new\r\ntail");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        assert!(snapshot(&root, "workspace", "../outside.ts").is_err());
        assert!(snapshot(&root, "workspace", ".git/config").is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
            assert!(snapshot(&root, "workspace", "escape/a.ts").is_err());
        }
    }

    #[test]
    fn closed_creation_is_exclusive_and_preserves_concurrent_content() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut file = target(&root, "nested/new.ts", "", "created");
        file.expected_hash = "missing".into();
        let result = apply(&root, operation(&root, vec![file.clone()])).unwrap();
        assert_eq!(result.status, "applied");
        assert_eq!(
            std::fs::read_to_string(root.join("nested/new.ts")).unwrap(),
            "created"
        );
        assert_eq!(
            apply(&root, operation(&root, vec![file])).unwrap().status,
            "conflict"
        );
    }

    #[cfg(unix)]
    #[test]
    fn closed_read_only_files_stay_intact() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let path = root.join("readonly.ts");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let result = apply(
            &root,
            operation(&root, vec![target(&root, "readonly.ts", "old", "agent")]),
        )
        .unwrap();
        assert_eq!(result.status, "rejected");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert!(coding_workspace::write_coding_document_blocking(
            &root,
            CodingWriteDocumentRequest {
                root: root.to_string_lossy().into(),
                path: "readonly.ts".into(),
                content: "agent".into(),
                expected_hash: coding_workspace::document_hash(b"old"),
            }
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn shared_conditional_write_serializes_competing_same_hash_updates() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ts"), "old").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let threads = (0..2)
            .map(|index| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    coding_workspace::write_coding_document_blocking(
                        &root,
                        CodingWriteDocumentRequest {
                            root: root.to_string_lossy().into(),
                            path: "a.ts".into(),
                            content: format!("new-{index}"),
                            expected_hash: coding_workspace::document_hash(b"old"),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap().is_ok())
                .filter(|success| *success)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn fabricated_pending_identity_never_authorizes_a_disk_action() {
        let touched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marker = touched.clone();
        let result = coding_document_bridge::with_pending_action(
            "fake-operation".into(),
            "/fake-root".into(),
            "fake-session".into(),
            "fake-run".into(),
            move || {
                marker.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert!(!touched.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[test]
    fn a_closed_patch_can_move_and_delete_files_without_overwriting_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("source.ts"), "old").unwrap();
        std::fs::write(root.join("remove.ts"), "delete").unwrap();
        let mut moved = target(&root, "source.ts", "old", "new");
        moved.move_to = Some(root.join("renamed.ts").to_string_lossy().into());
        let mut removed = target(&root, "remove.ts", "delete", "");
        removed.delete = true;
        removed.edits.clear();
        let result = apply(&root, operation(&root, vec![moved, removed])).unwrap();
        assert_eq!(result.status, "applied");
        assert!(!root.join("source.ts").exists());
        assert!(!root.join("remove.ts").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("renamed.ts")).unwrap(),
            "new"
        );
        assert_eq!(result.files[0].hash.as_deref(), Some("missing"));
        assert!(result.files[0].moved_to.is_some());
        std::fs::write(root.join("source.ts"), "another").unwrap();
        let mut collision = target(&root, "source.ts", "another", "changed");
        collision.move_to = Some(root.join("renamed.ts").to_string_lossy().into());
        assert_eq!(
            apply(&root, operation(&root, vec![collision]))
                .unwrap()
                .status,
            "conflict"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "another"
        );
    }
}
