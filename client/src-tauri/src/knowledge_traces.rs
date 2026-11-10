//! Durable per-turn provenance. Evidence paths never grant filesystem access.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::State;

const MAX_TRACE_BYTES: u64 = 32 * 1024;
const PAGE_SIZE: usize = 50;
static WRITES: Mutex<()> = Mutex::new(());

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn trace_root(home: &Path, session: &str) -> Result<PathBuf, String> {
    if !valid_id(session) {
        return Err("知识来源记录的会话标识无效".into());
    }
    let parent = home.join("knowledge-traces");
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&parent)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("知识来源目录不能是符号链接".into());
    }
    crate::paths::harden_private_dir(&parent)?;
    let root = parent.join(hash(session));
    if let Ok(metadata) = std::fs::symlink_metadata(&root) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("知识来源会话目录无效".into());
        }
    }
    Ok(root)
}

fn validate_trace(trace: &Value) -> Result<(), String> {
    let sources = trace
        .get("selectedSources")
        .and_then(Value::as_array)
        .ok_or("知识来源选择格式无效")?;
    if sources.len() > 2
        || sources
            .iter()
            .any(|s| !matches!(s.as_str(), Some("personal" | "organization")))
    {
        return Err("知识来源选择格式无效".into());
    }
    if let Some(personal) = trace.get("personal") {
        let state = personal
            .get("state")
            .and_then(Value::as_str)
            .ok_or("知识检索状态无效")?;
        if !matches!(state, "idle" | "used" | "no-match" | "blocked" | "error") {
            return Err("只保存已结束的知识检索状态".into());
        }
        if state == "used" {
            let items = personal
                .get("items")
                .and_then(Value::as_array)
                .ok_or("知识来源片段无效")?;
            if items.len() > 5
                || items
                    .iter()
                    .any(|item| item.get("title").and_then(Value::as_str).is_none())
            {
                return Err("知识来源片段无效".into());
            }
            if personal
                .get("resultCount")
                .and_then(Value::as_u64)
                .is_none()
                || personal
                    .get("sourceCount")
                    .and_then(Value::as_u64)
                    .is_none()
                || personal.get("titles").and_then(Value::as_array).is_none()
            {
                return Err("知识来源计数无效".into());
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceRecord {
    prompt_id: String,
    trace: Value,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePage {
    traces: serde_json::Map<String, Value>,
    next_cursor: Option<String>,
}

fn save_at(home: &Path, session: &str, prompt: &str, trace: Value) -> Result<(), String> {
    if !valid_id(prompt) {
        return Err("知识来源记录的轮次标识无效".into());
    }
    validate_trace(&trace)?;
    let bytes = serde_json::to_vec(&TraceRecord {
        prompt_id: prompt.into(),
        trace,
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_TRACE_BYTES {
        return Err("单轮知识来源记录超过32KB，消息尚未发送".into());
    }
    let _guard = WRITES.lock().map_err(|_| "知识来源存储锁已损坏")?;
    let root = trace_root(home, session)?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    crate::paths::harden_private_dir(&root)?;
    crate::paths::write_private_file(&root.join(format!("{}.json", hash(prompt))), &bytes)
}

fn load_page_at(home: &Path, session: &str, cursor: Option<&str>) -> Result<TracePage, String> {
    if cursor.is_some_and(|c| c.len() != 64 || !c.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("知识来源分页标识无效".into());
    }
    let root = trace_root(home, session)?;
    if !root.exists() {
        return Ok(TracePage {
            traces: Default::default(),
            next_cursor: None,
        });
    }
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".json")?;
            (stem.len() == 64
                && stem.bytes().all(|b| b.is_ascii_hexdigit())
                && cursor.is_none_or(|c| stem > c))
            .then_some((stem.to_string(), entry.path()))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let next_cursor = (files.len() > PAGE_SIZE).then(|| files[PAGE_SIZE - 1].0.clone());
    let mut traces = serde_json::Map::new();
    for (stem, path) in files.into_iter().take(PAGE_SIZE) {
        let raw = crate::shell_fs::read_regular_file_bounded(&path, MAX_TRACE_BYTES)?;
        let record: TraceRecord = serde_json::from_slice(&raw)
            .map_err(|e| format!("知识来源记录损坏，原文件已保留：{e}"))?;
        if !valid_id(&record.prompt_id) || hash(&record.prompt_id) != stem {
            return Err("知识来源记录标识无效，原文件已保留".into());
        }
        validate_trace(&record.trace)?;
        traces.insert(record.prompt_id, record.trace);
    }
    Ok(TracePage {
        traces,
        next_cursor,
    })
}

#[tauri::command]
pub async fn knowledge_traces_load(
    state: State<'_, crate::commands::AppState>,
    session_id: String,
    cursor: Option<String>,
) -> Result<TracePage, String> {
    state.session_workspace(&session_id)?;
    // Records are published atomically. Reading a page can run alongside a
    // new turn's write without blocking it on the historical records.
    tokio::task::spawn_blocking(move || {
        load_page_at(
            &crate::paths::echo_agent_home_dir(),
            &session_id,
            cursor.as_deref(),
        )
    })
    .await
    .map_err(|error| format!("读取历史知识来源失败：{error}"))?
}
#[tauri::command]
pub fn knowledge_trace_save(
    state: State<'_, crate::commands::AppState>,
    session_id: String,
    prompt_id: String,
    trace: Value,
) -> Result<(), String> {
    state.session_workspace(&session_id)?;
    save_at(
        &crate::paths::echo_agent_home_dir(),
        &session_id,
        &prompt_id,
        trace,
    )
}

fn clone_filtered_at(
    home: &Path,
    source: &str,
    target: &str,
    retained: Option<&std::collections::HashSet<String>>,
) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|_| "知识来源存储锁已损坏")?;
    let from = trace_root(home, source)?;
    let to = trace_root(home, target)?;
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        return if retained.is_some() {
            Ok(())
        } else {
            Err("分叉知识来源目录已存在，原记录未覆盖".into())
        };
    }
    let stage = tempfile::Builder::new()
        .prefix(".fork-")
        .tempdir_in(from.parent().unwrap())
        .map_err(|e| e.to_string())?;
    crate::paths::harden_private_dir(stage.path())?;
    for entry in std::fs::read_dir(&from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        let Some(stem) = text.strip_suffix(".json") else {
            continue;
        };
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        // Validate before sharing an immutable inode. Future updates publish a
        // new inode, so fork evidence stays a snapshot and survives deletion.
        let bytes = crate::shell_fs::read_regular_file_bounded(&entry.path(), MAX_TRACE_BYTES)?;
        let record: TraceRecord = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        validate_trace(&record.trace)?;
        if hash(&record.prompt_id) != stem {
            return Err("原会话知识来源标识无效，原记录已保留".into());
        }
        if retained.is_some_and(|ids| !ids.contains(&record.prompt_id)) {
            continue;
        }
        std::fs::hard_link(entry.path(), stage.path().join(name)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(stage.path(), &to).map_err(|e| e.to_string())?;
    Ok(())
}

fn clone_at(home: &Path, source: &str, target: &str) -> Result<(), String> {
    clone_filtered_at(home, source, target, None)
}

pub(crate) fn clone_session_filtered(
    source: &str,
    target: &str,
    retained: &std::collections::HashSet<String>,
) -> Result<(), String> {
    clone_filtered_at(
        &crate::paths::echo_agent_home_dir(),
        source,
        target,
        Some(retained),
    )
}

pub(crate) fn clone_session(source: &str, target: &str) -> Result<(), String> {
    clone_at(&crate::paths::echo_agent_home_dir(), source, target)
}

pub(crate) fn forget_session(session: &str) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|_| "知识来源存储锁已损坏")?;
    let root = trace_root(&crate::paths::echo_agent_home_dir(), session)?;
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_continuation_copies_only_retained_turns_and_retry_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        save_at(
            home.path(),
            "source",
            "kept",
            serde_json::json!({"selectedSources":[]}),
        )
        .unwrap();
        save_at(
            home.path(),
            "source",
            "later",
            serde_json::json!({"selectedSources":[]}),
        )
        .unwrap();
        let ids = std::collections::HashSet::from(["kept".into()]);
        clone_filtered_at(home.path(), "source", "child", Some(&ids)).unwrap();
        clone_filtered_at(home.path(), "source", "child", Some(&ids)).unwrap();
        let records = load_page_at(home.path(), "child", None).unwrap();
        assert!(records.traces.contains_key("kept"));
        assert!(!records.traces.contains_key("later"));
    }
    #[test]
    fn fork_sources_keep_original_evidence_after_source_changes_and_deletion() {
        let home = tempfile::tempdir().unwrap();
        let trace = serde_json::json!({"selectedSources":["organization"],"organization":{"state":"available"}});
        save_at(home.path(), "original", "p", trace.clone()).unwrap();
        clone_at(home.path(), "original", "fork").unwrap();
        save_at(
            home.path(),
            "original",
            "p",
            serde_json::json!({"selectedSources":[]}),
        )
        .unwrap();
        std::fs::remove_dir_all(trace_root(home.path(), "original").unwrap()).unwrap();
        assert_eq!(
            load_page_at(home.path(), "fork", None).unwrap().traces["p"],
            trace
        );
    }

    #[test]
    fn provenance_roundtrips_all_history_in_bounded_pages_without_granting_files() {
        let home = tempfile::tempdir().unwrap();
        for turn in 0..115 {
            save_at(home.path(), "session", &format!("prompt-{turn}"), serde_json::json!({"selectedSources":["personal"],"personal":{"state":"used","resultCount":1,"sourceCount":1,"titles":["A"],"items":[{"title":"A","path":"/ungranted/A.md","snippet":"old evidence","startLine":4}]}})).unwrap();
        }
        let mut all = serde_json::Map::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let page = load_page_at(home.path(), "session", cursor.as_deref()).unwrap();
            assert!(page.traces.len() <= PAGE_SIZE);
            all.extend(page.traces);
            pages += 1;
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(all.len(), 115);
        assert_eq!(pages, 3);
        assert_eq!(
            all["prompt-0"]["personal"]["items"][0]["snippet"],
            "old evidence"
        );
        assert!(save_at(
            home.path(),
            "session",
            "bad",
            serde_json::json!({"selectedSources":["invalid"]})
        )
        .is_err());
        assert!(load_page_at(home.path(), "session", Some("../escape")).is_err());
    }
}
