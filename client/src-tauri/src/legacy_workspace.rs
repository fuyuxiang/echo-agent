//! Preserve the write boundary of old external operations whose outcome is unknown.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

pub(crate) const LEASE_FILE: &str = "coding-external-tool-leases.json";
const MAX_LEASE_BYTES: u64 = 2 * 1024 * 1024;
static VERIFY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExternalOperation {
    workspace: PathBuf,
    operation_id: String,
    tool_name: String,
    #[serde(default)]
    terminal: bool,
    #[serde(flatten)]
    history: BTreeMap<String, serde_json::Value>,
}

fn lease_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join(LEASE_FILE)
}

fn read_operations(path: &Path) -> Result<Vec<ExternalOperation>, String> {
    use std::io::ErrorKind;
    match crate::shell_fs::read_regular_file_bounded_io(path, MAX_LEASE_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("无法读取旧版外部操作记录：{error}")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("无法读取旧版外部操作记录：{error}")),
    }
}

fn save_operations(path: &Path, operations: &[ExternalOperation]) -> Result<(), String> {
    let bytes = serde_json::to_vec(operations).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_LEASE_BYTES {
        return Err("旧版外部操作记录超过大小上限".into());
    }
    crate::paths::write_private_file(path, &bytes)
}

/// An older backup cannot supersede a local operation with an uncertain outcome.
pub(crate) fn merge_restored_operations(staged: &Path, current: &Path) -> Result<(), String> {
    let mut records = read_operations(current)?;
    for restored in read_operations(staged)? {
        if let Some(local) = records
            .iter()
            .find(|item| item.operation_id == restored.operation_id)
        {
            if local.workspace != restored.workspace
                || local.tool_name != restored.tool_name
                || ["sessionId", "runId"]
                    .iter()
                    .any(|id| local.history.get(*id) != restored.history.get(*id))
            {
                return Err("旧版外部操作记录标识冲突，原有目录限制已保留".into());
            }
        } else {
            records.push(restored);
        }
    }
    save_operations(staged, &records)
}

fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        // Normalize drive and UNC aliases and compare each component rather
        // than using a prefix on the full string.
        let path = dunce::simplified(path);
        let root = dunce::simplified(root);
        root.components().zip(path.components()).all(|(a, b)| {
            a.as_os_str().to_string_lossy().to_lowercase()
                == b.as_os_str().to_string_lossy().to_lowercase()
        }) && root.components().count() <= path.components().count()
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

fn related_path(a: &Path, b: &Path) -> bool {
    let canonicalize = |path: &Path| {
        crate::shell_fs::canonicalize_candidate(path, true).unwrap_or_else(|_| path.to_owned())
    };
    let a = canonicalize(a);
    let b = canonicalize(b);
    within(&a, &b) || within(&b, &a)
}

fn check_operations(operations: &[ExternalOperation], path: &Path) -> Result<(), String> {
    if let Some(operation) = operations
        .iter()
        .find(|lease| !lease.terminal && related_path(path, &lease.workspace))
    {
        return Err(format!(
            "旧版外部操作状态待核验，目录 {} 暂停执行和写入。请在「设置 → 数据管理 → 本机数据」中核验待处理操作。",
            operation.workspace.display()
        ));
    }
    Ok(())
}

pub(crate) fn ensure_workspace_available(path: &Path) -> Result<(), String> {
    check_operations(&read_operations(&lease_path())?, path)
}

fn archived_session_ids(home: &Path) -> BTreeSet<String> {
    let mut sessions = BTreeSet::new();
    for entry in walkdir::WalkDir::new(home.join("coding"))
        .follow_root_links(false)
        .min_depth(3)
        .max_depth(3)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "task.json")
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchivedTask {
            session_id: Option<String>,
        }
        let task = crate::shell_fs::read_regular_file_bounded(entry.path(), 8 * 1024 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ArchivedTask>(&bytes).ok());
        if let Some(session_id) = task.and_then(|task| task.session_id) {
            sessions.insert(session_id);
        }
    }
    sessions
}

#[tauri::command]
pub(crate) async fn legacy_workspace_sessions(
    window: tauri::WebviewWindow,
) -> Result<BTreeSet<String>, String> {
    if window.label() != "main" {
        return Err("请在主窗口查看旧版会话".into());
    }
    tauri::async_runtime::spawn_blocking(|| {
        archived_session_ids(&crate::paths::echo_agent_home_dir())
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn pending_external_operations(
    window: tauri::WebviewWindow,
) -> Result<Vec<serde_json::Value>, String> {
    if window.label() != "main" {
        return Err("请在主窗口查看旧版操作记录".into());
    }
    Ok(read_operations(&lease_path())?
        .into_iter()
        .filter(|record| !record.terminal)
        .map(|record| {
            serde_json::json!({
                "workspace": record.workspace,
                "operationId": record.operation_id,
                "toolName": record.tool_name,
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn confirm_external_operation_stopped(
    window: tauri::WebviewWindow,
    operation_id: String,
    note: String,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("请在主窗口核验旧版操作".into());
    }
    let note = note.trim();
    if !(8..=2000).contains(&note.chars().count()) {
        return Err("请填写 8～2000 字的核验依据".into());
    }
    let path = lease_path();
    let record = read_operations(&path)?
        .into_iter()
        .find(|record| record.operation_id == operation_id && !record.terminal)
        .ok_or("该操作已核验或记录已变化，请刷新")?;
    let (send, receive) = tokio::sync::oneshot::channel();
    window.dialog()
        .message(format!(
            "请确认工具 {} 的实际执行已结束，且已检查目录 {} 中可能留下的更改。\n\n核验依据：{note}",
            record.tool_name, record.workspace.display()
        ))
        .title("确认旧版外部操作已结束")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("已核实，解除限制".into(), "取消".into()))
        .show(move |accepted| { let _ = send.send(accepted); });
    if !receive.await.unwrap_or(false) {
        return Err("已取消核验，目录限制仍有效".into());
    }
    let _lock = VERIFY_LOCK.lock().await;
    let mut operations = read_operations(&path)?;
    let current = operations
        .iter_mut()
        .find(|operation| **operation == record)
        .ok_or("记录已变化，请重新核验")?;
    current.terminal = true;
    current.history.insert(
        "verifiedStopped".into(),
        serde_json::json!({
            "note": note,
            "confirmedAt": chrono::Utc::now().to_rfc3339(),
        }),
    );
    save_operations(&path, &operations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_tasks_identify_legacy_sessions_independently_of_title_and_path() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("coding/0123456789abcdef/task-id");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("task.json"),
            br#"{"sessionId":"legacy-session"}"#,
        )
        .unwrap();
        std::fs::write(
            folder.join("plan.json"),
            br#"{"sessionId":"other-session"}"#,
        )
        .unwrap();
        assert_eq!(
            archived_session_ids(home.path()),
            BTreeSet::from(["legacy-session".into()])
        );
    }

    #[test]
    fn unresolved_operation_blocks_only_related_paths() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let mut operations = vec![ExternalOperation {
            workspace: workspace.clone(),
            operation_id: "old-op".into(),
            tool_name: "old-tool".into(),
            terminal: false,
            history: BTreeMap::new(),
        }];
        assert!(check_operations(&operations, &workspace.join("child")).is_err());
        assert!(check_operations(&operations, temp.path()).is_err());
        assert!(check_operations(&operations, &temp.path().join("workspace-other")).is_ok());
        operations[0].terminal = true;
        assert!(check_operations(&operations, &workspace).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn new_file_under_an_alias_is_subject_to_the_workspace_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&workspace).unwrap();
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        assert!(related_path(&alias.join("new.txt"), &workspace));
    }
}
