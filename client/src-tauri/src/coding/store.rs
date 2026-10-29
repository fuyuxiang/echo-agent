//! File-backed persistence for coding tasks. Mirrors the JSON/JSONL layout
//! already used by `sessions.rs` instead of introducing a database.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

/// Per-workspace state root, keyed by a hash of the canonical repository path
/// so two checkouts of the same project never share task state.
pub fn workspace_dir(root: &Path) -> PathBuf {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hash: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    crate::paths::echo_agent_home_dir()
        .join("coding")
        .join(hash)
}

/// Validate a renderer-provided task key before using it as a path component.
/// UUIDs are used in production; the wider safe set preserves older fixtures.
pub fn validate_task_id(task_id: &str) -> Result<(), String> {
    if task_id.is_empty()
        || task_id.len() > 100
        || task_id == "."
        || task_id == ".."
        || !task_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("任务标识无效".into());
    }
    Ok(())
}

pub fn task_dir(root: &Path, task_id: &str) -> PathBuf {
    // Remain path-safe if a future internal call site forgets to validate. IPC
    // entry points still validate explicitly so users receive a useful error.
    let component = if validate_task_id(task_id).is_ok() {
        task_id
    } else {
        ".invalid-task-id"
    };
    workspace_dir(root).join(component)
}

pub fn tasks_index_path(root: &Path) -> PathBuf {
    workspace_dir(root).join("tasks.json")
}

/// Paths to the workspace-level cross-file index files introduced in phase 2.
/// They live next to `tasks.json` so they are reachable across all tasks of the
/// same workspace and so an empty state is detected by their absence.
#[derive(Clone, Debug)]
pub struct IndexPaths {
    pub symbols: PathBuf,
    pub file_index: PathBuf,
    pub symbol_overrides: PathBuf,
}

pub fn index_paths(root: &Path) -> IndexPaths {
    let base = workspace_dir(root);
    IndexPaths {
        symbols: base.join("symbols.jsonl"),
        file_index: base.join("file_index.json"),
        symbol_overrides: base.join("symbol-overrides"),
    }
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| format!("无法创建目录：{error}"))
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    path.with_file_name(name)
}

fn open_lock(path: &Path) -> Option<std::fs::File> {
    ensure_parent(path).ok()?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path(path))
        .ok()
}

/// Read a JSON document, treating a missing or corrupt file as absent so a
/// damaged task file can never crash the workbench.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    // A read must not create persistence directories. Besides being a
    // surprising side effect, opening a lock first recreated a deleted task
    // directory when list_tasks filtered a stale index entry.
    if !path.is_file() {
        return None;
    }
    // Do not call `open_lock` here: its write-path behavior creates the
    // parent. If deletion wins the race after is_file(), this open simply
    // fails and still cannot recreate the removed task directory.
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path(path))
        .ok()?;
    lock.lock_shared().ok()?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "Cannot read persisted coding data; original file retained");
            return None;
        }
    };
    let value = match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "Cannot decode persisted coding data; original file retained");
            None
        }
    };
    let _ = lock.unlock();
    value
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    ensure_parent(path)?;
    let lock = open_lock(path).ok_or_else(|| "无法创建持久化锁".to_string())?;
    lock.lock_exclusive()
        .map_err(|error| format!("无法锁定持久化文件：{error}"))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| format!("序列化失败：{error}"))?;
    let staging = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    let result = (|| {
        std::fs::write(&staging, bytes).map_err(|error| format!("写入临时文件失败：{error}"))?;
        crate::paths::replace_file_atomically(&staging, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    let _ = lock.unlock();
    result
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    ensure_parent(path)?;
    let staging = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
    let result = std::fs::write(&staging, bytes)
        .map_err(|error| format!("写入临时文件失败：{error}"))
        .and_then(|()| crate::paths::replace_file_atomically(&staging, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result
}

/// Update a JSON document while retaining its exclusive lock for the complete
/// read/modify/write transaction. Separate `read_json` and `write_json` calls
/// cannot provide this lost-update protection.
pub fn update_json<T, R>(
    path: &Path,
    update: impl FnOnce(&mut T) -> Result<R, String>,
) -> Result<R, String>
where
    T: DeserializeOwned + Serialize + Default,
{
    ensure_parent(path)?;
    let lock = open_lock(path).ok_or_else(|| "无法创建持久化锁".to_string())?;
    lock.lock_exclusive()
        .map_err(|error| format!("无法锁定持久化文件：{error}"))?;
    let result = (|| {
        let mut value = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("持久化文件格式无效，已停止覆盖：{error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
            Err(error) => return Err(format!("读取持久化文件失败：{error}")),
        };
        let output = update(&mut value)?;
        let bytes =
            serde_json::to_vec_pretty(&value).map_err(|error| format!("序列化失败：{error}"))?;
        let staging = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
        if let Err(error) = std::fs::write(&staging, bytes)
            .map_err(|error| format!("写入临时文件失败：{error}"))
            .and_then(|()| crate::paths::replace_file_atomically(&staging, path))
        {
            let _ = std::fs::remove_file(&staging);
            return Err(error);
        }
        Ok(output)
    })();
    let _ = lock.unlock();
    result
}

/// Serialize every state transition for one task. The callback form keeps the
/// transaction guard scoped above the individual task/index file locks.
pub fn with_task_transaction<T>(
    root: &Path,
    task_id: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    validate_task_id(task_id)?;
    let lock_path = workspace_dir(root)
        .join(".task-locks")
        .join(format!("{task_id}.lock"));
    ensure_parent(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("无法创建任务事务锁：{error}"))?;
    lock.lock_exclusive()
        .map_err(|error| format!("无法锁定任务状态：{error}"))?;
    let result = operation();
    let _ = lock.unlock();
    result
}

fn with_index_lock<T>(
    root: &Path,
    exclusive: bool,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let lock_path = workspace_dir(root).join("index.transaction.lock");
    ensure_parent(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("无法创建索引事务锁：{error}"))?;
    if exclusive {
        lock.lock_exclusive()
    } else {
        lock.lock_shared()
    }
    .map_err(|error| format!("无法锁定符号索引：{error}"))?;
    let result = operation();
    let _ = lock.unlock();
    result
}

pub fn with_index_read<T>(
    root: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_index_lock(root, false, operation)
}

pub fn with_index_transaction<T>(
    root: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_index_lock(root, true, operation)
}

pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    use std::io::Write;
    ensure_parent(path)?;
    let line = serde_json::to_string(value).map_err(|error| format!("序列化失败：{error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("打开失败：{error}"))?;
    file.lock_exclusive()
        .map_err(|error| format!("无法锁定事件日志：{error}"))?;
    let result = writeln!(file, "{line}").map_err(|error| format!("写入失败：{error}"));
    if result.is_ok() {
        let _ = file.sync_data();
    }
    let _ = file.unlock();
    result
}

/// Read an append-only log, skipping lines that failed to serialize or were
/// truncated by an interrupted write.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        id: String,
        count: u32,
    }

    #[test]
    fn workspace_hash_is_stable_and_path_scoped() {
        let a = workspace_dir(Path::new("/tmp/repo-a"));
        let b = workspace_dir(Path::new("/tmp/repo-b"));
        assert_ne!(a, b);
        assert_eq!(a, workspace_dir(Path::new("/tmp/repo-a")));
    }

    #[test]
    fn task_id_never_escapes_workspace_storage() {
        let root = Path::new("/tmp/example-workspace");
        let base = workspace_dir(root);
        assert!(validate_task_id("task-1").is_ok());
        assert!(validate_task_id("../other/task").is_err());
        assert!(validate_task_id("/tmp/task").is_err());
        assert_eq!(
            task_dir(root, "../other/task"),
            base.join(".invalid-task-id")
        );
    }

    #[test]
    fn update_json_keeps_read_modify_write_under_one_lock() {
        let dir = std::env::temp_dir().join(format!("coding-store-{}", uuid::Uuid::now_v7()));
        let path = dir.join("counter.json");
        update_json::<u32, _>(&path, |value| {
            *value += 1;
            Ok(())
        })
        .unwrap();
        update_json::<u32, _>(&path, |value| {
            *value += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(read_json::<u32>(&path), Some(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_json_never_replaces_a_corrupt_document_with_defaults() {
        let dir = std::env::temp_dir().join(format!("coding-store-{}", uuid::Uuid::now_v7()));
        let path = dir.join("state.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"{broken").unwrap();
        let error = update_json::<Vec<String>, _>(&path, |values| {
            values.push("new".into());
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("已停止覆盖"));
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_roundtrip_and_missing_file_is_none() {
        let dir = std::env::temp_dir().join(format!("coding-store-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.json");
        assert!(read_json::<Sample>(&path).is_none());
        let value = Sample {
            id: "t1".into(),
            count: 3,
        };
        write_json(&path, &value).unwrap();
        assert_eq!(read_json::<Sample>(&path), Some(value));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn jsonl_appends_in_order_and_skips_corrupt_lines() {
        let dir = std::env::temp_dir().join(format!("coding-store-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        append_jsonl(
            &path,
            &Sample {
                id: "a".into(),
                count: 1,
            },
        )
        .unwrap();
        append_jsonl(
            &path,
            &Sample {
                id: "b".into(),
                count: 2,
            },
        )
        .unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n{{bad json}}\n",
                std::fs::read_to_string(&path).unwrap().trim()
            ),
        )
        .unwrap();
        let items: Vec<Sample> = read_jsonl(&path);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a");
        assert_eq!(items[1].id, "b");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn index_paths_are_workspace_level_and_share_parent_with_workspace_dir() {
        let root = Path::new("/tmp/repo-index");
        let paths = index_paths(root);
        let dir = workspace_dir(root);
        assert_eq!(paths.symbols.parent(), Some(dir.as_path()));
        assert_eq!(paths.file_index.parent(), Some(dir.as_path()));
        assert_eq!(paths.symbols.file_name().unwrap(), "symbols.jsonl");
        assert_eq!(paths.file_index.file_name().unwrap(), "file_index.json");
    }
}
