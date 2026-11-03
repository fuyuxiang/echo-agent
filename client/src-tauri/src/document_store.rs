//! Durable editable sources. Revisions protect manual and Agent edits equally.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
static DELETED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
fn owner_key(home: &Path, session: &str) -> String {
    format!("{}\0{session}", home.display())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentVersion {
    pub revision: u64,
    pub saved_at: u64,
    pub actor: String,
    pub document: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditableDocument {
    pub id: String,
    pub session_id: String,
    pub path: String,
    pub title: String,
    pub revision: u64,
    pub exported_revision: u64,
    pub file_sha256: String,
    pub byte_size: u64,
    pub document: Value,
    pub saved_at: u64,
    pub actor: String,
    pub history: Vec<DocumentVersion>,
    #[serde(default)]
    pub pending_export: Option<(u64, String, u64)>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub external_change: bool,
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn identity(session: &str, path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{session}\0{}", path.to_string_lossy()).as_bytes())
    )
}
fn store_path(home: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("文档标识无效".into());
    }
    let root = home.join("document-workspaces");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&root)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("文档存储目录不能是符号链接".into());
    }
    crate::paths::harden_private_dir(&root)?;
    Ok(root.join(format!("{id}.json")))
}
pub fn read(home: &Path, id: &str) -> Result<Option<EditableDocument>, String> {
    let path = store_path(home, id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(&path, 12 * 1024 * 1024)?;
    let value: EditableDocument =
        serde_json::from_slice(&bytes).map_err(|e| format!("编辑稿记录损坏，原文件已保留：{e}"))?;
    if value.id != id {
        return Err("编辑稿归属不一致".into());
    }
    validate(&value.document)?;
    Ok(Some(value))
}
pub fn write(home: &Path, value: &EditableDocument) -> Result<(), String> {
    let deleted = DELETED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map_err(|_| "文档存储状态异常")?;
    if deleted.contains(&owner_key(home, &value.session_id)) {
        return Err("该任务已删除，编辑稿未再写入".into());
    }
    let path = store_path(home, &value.id)?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > 12 * 1024 * 1024 {
        return Err("文档历史记录超出保存范围".into());
    }
    let mut stage =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(|e| e.to_string())?;
    crate::paths::harden_private_file(stage.path())?;
    stage
        .write_all(&bytes)
        .and_then(|_| stage.as_file().sync_all())
        .map_err(|e| format!("编辑稿保存失败：{e}"))?;
    stage
        .persist(&path)
        .map_err(|e| format!("编辑稿保存失败：{}", e.error))?;
    Ok(())
}
pub fn validate(document: &Value) -> Result<(), String> {
    if serde_json::to_vec(document)
        .map_err(|e| e.to_string())?
        .len()
        > 2 * 1024 * 1024
    {
        return Err("文档内容超过 2 MiB".into());
    }
    fn node(
        value: &Value,
        parent: &str,
        depth: usize,
        count: &mut usize,
        text_size: &mut usize,
    ) -> Result<(), String> {
        *count += 1;
        if depth > 32 || *count > 20000 {
            return Err("文档结构过深或节点过多".into());
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or("文档节点缺少类型")?;
        let allowed = match parent {
            "root" => kind == "doc",
            "paragraph" | "heading" | "codeBlock" => matches!(kind, "text" | "hardBreak"),
            "bulletList" | "orderedList" => kind == "listItem",
            "table" => kind == "tableRow",
            "tableRow" => matches!(kind, "tableCell" | "tableHeader"),
            _ => matches!(
                kind,
                "paragraph"
                    | "heading"
                    | "bulletList"
                    | "orderedList"
                    | "blockquote"
                    | "codeBlock"
                    | "horizontalRule"
                    | "table"
            ),
        };
        if !allowed {
            return Err(format!("不支持的文档节点：{kind}"));
        }
        if value
            .get("attrs")
            .is_some_and(|attrs| !attrs.is_object() && !attrs.is_null())
        {
            return Err("文档节点属性无效".into());
        }
        if kind == "heading"
            && value
                .pointer("/attrs/level")
                .is_some_and(|level| !level.as_u64().is_some_and(|level| (1..=3).contains(&level)))
        {
            return Err("文档标题仅支持 1 至 3 级".into());
        }
        if kind == "orderedList"
            && value.pointer("/attrs/start").is_some_and(|start| {
                !start
                    .as_u64()
                    .is_some_and(|start| (1..=100000).contains(&start))
            })
        {
            return Err("有序列表起始编号无效".into());
        }
        if matches!(kind, "paragraph" | "heading")
            && value.pointer("/attrs/textAlign").is_some_and(|align| {
                !align.is_null()
                    && !matches!(
                        align.as_str(),
                        Some("left" | "center" | "right" | "justify")
                    )
            })
        {
            return Err("段落对齐方式无效".into());
        }
        if kind == "text"
            && value
                .get("text")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err("文字节点不能为空".into());
        }
        if kind == "text" {
            *text_size += value["text"].as_str().unwrap().encode_utf16().count();
            if *text_size > 1000000 {
                return Err("文档内容过长".into());
            }
        }
        if let Some(marks) = value.get("marks") {
            for mark in marks.as_array().ok_or("文字格式无效")? {
                let kind = mark.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(
                    kind,
                    "bold" | "italic" | "underline" | "strike" | "code" | "link"
                ) {
                    return Err("不支持的文字格式".into());
                }
                if kind == "link" {
                    let href = mark
                        .pointer("/attrs/href")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if href.len() > 2048
                        || !["https://", "http://", "mailto:"]
                            .iter()
                            .any(|prefix| href.starts_with(prefix))
                    {
                        return Err("文档链接无效".into());
                    }
                }
            }
        }
        if matches!(kind, "tableCell" | "tableHeader") {
            let rowspan = value
                .pointer("/attrs/rowspan")
                .map_or(Some(1), Value::as_u64)
                .ok_or("表格合并属性无效")?;
            let colspan = value
                .pointer("/attrs/colspan")
                .map_or(Some(1), Value::as_u64)
                .ok_or("表格合并属性无效")?;
            if rowspan != 1 || !(1..=32).contains(&colspan) {
                return Err("暂不支持跨行合并的表格".into());
            }
        }
        if let Some(content) = value.get("content") {
            let children = content.as_array().ok_or("文档内容格式无效")?;
            if matches!(kind, "text" | "hardBreak" | "horizontalRule") && !children.is_empty() {
                return Err("文字节点不能包含段落".into());
            }
            if matches!(kind, "table" | "tableRow" | "bulletList" | "orderedList")
                && children.is_empty()
            {
                return Err("表格或列表不能为空".into());
            }
            if (kind == "table" && children.len() > 1000)
                || (kind == "tableRow" && children.len() > 32)
            {
                return Err("表格超过编辑范围".into());
            }
            for child in children {
                node(child, kind, depth + 1, count, text_size)?;
            }
            if kind == "table" {
                let mut width = None;
                for row in children {
                    let cells = row
                        .get("content")
                        .and_then(Value::as_array)
                        .ok_or("表格行不能为空")?;
                    let columns: u64 = cells
                        .iter()
                        .map(|cell| {
                            cell.pointer("/attrs/colspan")
                                .and_then(Value::as_u64)
                                .unwrap_or(1)
                        })
                        .sum();
                    if columns == 0
                        || columns > 32
                        || width.is_some_and(|expected| expected != columns)
                    {
                        return Err("表格各行列数不一致".into());
                    }
                    width = Some(columns);
                }
            }
        } else if matches!(
            kind,
            "doc"
                | "table"
                | "tableRow"
                | "tableCell"
                | "tableHeader"
                | "listItem"
                | "bulletList"
                | "orderedList"
        ) {
            return Err("文档节点缺少内容".into());
        }
        Ok(())
    }
    node(document, "root", 0, &mut 0, &mut 0)
}
pub fn revise(
    value: &mut EditableDocument,
    expected: u64,
    document: Value,
    actor: &str,
) -> Result<(), String> {
    validate(&document)?;
    if value.revision != expected {
        return Err(format!(
            "DOCUMENT_CONFLICT:文档已有新修改，当前版本为 {}；你的编辑内容已保留",
            value.revision
        ));
    }
    if value.document == document {
        return Ok(());
    }
    value.history.push(DocumentVersion {
        revision: value.revision,
        saved_at: value.saved_at,
        actor: value.actor.clone(),
        document: value.document.clone(),
    });
    while value.history.len() > 20
        || serde_json::to_vec(&value.history)
            .map_err(|e| e.to_string())?
            .len()
            > 8 * 1024 * 1024
    {
        value.history.remove(0);
    }
    value.revision += 1;
    value.document = document;
    value.saved_at = now();
    value.actor = actor.into();
    Ok(())
}

pub fn forget_session(home: &Path, session: &str) -> Result<(), String> {
    let mut deleted = DELETED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map_err(|_| "文档存储状态异常")?;
    deleted.insert(owner_key(home, session));
    let root = home.join("document-workspaces");
    if !root.exists() {
        return Ok(());
    }
    if std::fs::symlink_metadata(&root)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("文档存储目录不能是符号链接".into());
    }
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = crate::shell_fs::read_regular_file_bounded(&path, 12 * 1024 * 1024)?;
        let record: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if record.get("sessionId").and_then(Value::as_str) == Some(session) {
            std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Publish without overwriting a file another application has just replaced.
/// The previous file is checked again after moving it aside; failed rollback
/// retains the backup and reports its exact recovery path.
pub fn install_revision(
    source: &Path,
    destination: &Path,
    expected_hash: &str,
) -> Result<(), String> {
    let hash = |path: &Path| -> Result<String, String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(crate::shell_fs::read_regular_file_bounded(
                path,
                50 * 1024 * 1024
            )?)
        ))
    };
    if hash(destination)? != expected_hash {
        return Err("DOCUMENT_EXTERNAL_CHANGE:原文件已修改，未覆盖".into());
    }
    let parent = destination.parent().ok_or("文档目录无效")?;
    let stage = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    std::fs::copy(source, stage.path()).map_err(|error| error.to_string())?;
    stage
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let backup = parent.join(format!(
        ".echo-document-backup-{}",
        stage.path().file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(destination, &backup)
        .map_err(|error| format!("无法保护原文档，编辑稿已保留：{error}"))?;
    let restore = || -> Result<(), String> {
        std::fs::hard_link(&backup, destination).map_err(|error| {
            format!("未覆盖当前文件。原件已保存在 {}：{error}", backup.display())
        })?;
        std::fs::remove_file(&backup).map_err(|error| error.to_string())
    };
    let protected_hash = match hash(&backup) {
        Ok(hash) => hash,
        Err(error) => {
            let rollback = restore();
            return Err(format!(
                "无法核对原文档，编辑稿已保留：{error}{}",
                rollback
                    .err()
                    .map(|detail| format!("；{detail}"))
                    .unwrap_or_default()
            ));
        }
    };
    if protected_hash != expected_hash {
        restore()?;
        return Err("DOCUMENT_EXTERNAL_CHANGE:原件在保存期间发生变化，未覆盖".into());
    }
    if let Err(error) = stage.persist_noclobber(destination) {
        let rollback = restore();
        return Err(format!(
            "文档未更新，编辑稿已保留：{}{}",
            error.error,
            rollback
                .err()
                .map(|detail| format!("；{detail}"))
                .unwrap_or_default()
        ));
    }
    std::fs::remove_file(&backup)
        .map_err(|error| format!("文档已更新，旧版备份保留在 {}：{error}", backup.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn text(value: &str) -> Value {
        json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":value}]}]})
    }
    fn record() -> EditableDocument {
        EditableDocument {
            id: "a".repeat(64),
            session_id: "s".into(),
            path: "/x.docx".into(),
            title: "文档".into(),
            revision: 1,
            exported_revision: 1,
            file_sha256: "b".repeat(64),
            byte_size: 1,
            document: text("原稿"),
            saved_at: now(),
            actor: "生成".into(),
            history: vec![],
            pending_export: None,
            external_change: false,
        }
    }
    #[test]
    fn preserves_edits_and_history_across_restarts() {
        let home = tempfile::tempdir().unwrap();
        let mut value = record();
        revise(&mut value, 1, text("手动修改"), "手动编辑").unwrap();
        write(home.path(), &value).unwrap();
        let restored = read(home.path(), &value.id).unwrap().unwrap();
        assert_eq!(restored.document, text("手动修改"));
        assert_eq!(restored.history[0].document, text("原稿"));
        assert_eq!(restored.exported_revision, 1);
    }
    #[test]
    fn rejects_stale_agent_revision_without_changing_manual_edits() {
        let mut value = record();
        revise(&mut value, 1, text("用户新修改"), "手动编辑").unwrap();
        assert!(revise(&mut value, 1, text("Agent旧结果"), "Agent")
            .unwrap_err()
            .starts_with("DOCUMENT_CONFLICT:"));
        assert_eq!(value.document, text("用户新修改"));
    }
    #[test]
    fn rejects_malformed_nodes_and_unsafe_links() {
        assert!(validate(&json!({"type":"doc","content":[{"type":"script"}]})).is_err());
        assert!(validate(&json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"x","marks":[{"type":"link","attrs":{"href":"javascript:alert(1)"}}]}]}]})).is_err());
        for block in [
            json!({"type":"table","content":[]}),
            json!({"type":"heading","attrs":{"level":6}}),
            json!({"type":"orderedList","attrs":{"start":-1},"content":[]}),
        ] {
            assert!(validate(&json!({"type":"doc","content":[block]})).is_err());
        }
    }
    #[test]
    fn bounds_history_and_restores_as_a_new_revision() {
        let mut value = record();
        for i in 0..30 {
            let revision = value.revision;
            revise(&mut value, revision, text(&format!("版本{i}")), "手动编辑").unwrap();
        }
        assert_eq!(value.history.len(), 20);
        let previous = value.history[0].document.clone();
        let revision = value.revision;
        revise(&mut value, revision, previous.clone(), "恢复历史").unwrap();
        assert_eq!(value.revision, revision + 1);
        assert_eq!(value.document, previous);
    }
    #[test]
    fn publishes_only_over_the_expected_file_and_preserves_external_edits() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("file.docx");
        let source = directory.path().join("new.docx");
        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&source, b"updated").unwrap();
        let original_hash = format!("{:x}", Sha256::digest(b"original"));
        install_revision(&source, &target, &original_hash).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"updated");
        std::fs::write(&target, b"external modification").unwrap();
        assert!(install_revision(&source, &target, &original_hash)
            .unwrap_err()
            .contains("DOCUMENT_EXTERNAL_CHANGE"));
        assert_eq!(std::fs::read(&target).unwrap(), b"external modification");
    }
    #[test]
    fn document_identity_is_scoped_to_the_session() {
        assert_ne!(
            identity("a", Path::new("/work/report.docx")),
            identity("b", Path::new("/work/report.docx"))
        );
    }

    #[test]
    fn deleted_sessions_remove_only_their_sources_and_reject_late_saves() {
        let home = tempfile::tempdir().unwrap();
        let value = record();
        write(home.path(), &value).unwrap();
        let mut other = record();
        other.id = "c".repeat(64);
        other.session_id = "other".into();
        write(home.path(), &other).unwrap();
        forget_session(home.path(), "s").unwrap();
        assert!(read(home.path(), &value.id).unwrap().is_none());
        assert!(read(home.path(), &other.id).unwrap().is_some());
        assert!(write(home.path(), &value).is_err());
    }
}
