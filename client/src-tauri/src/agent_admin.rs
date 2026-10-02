//! Higher-level EchoAgent admin extensions — bridges the upstream `echo.agent/*` methods that
//! drive EchoAgent-equivalent features:
//!
//! - **Memory** (资料库): read/rewrite canonical global + hashed workspace
//!   memory, flush active sessions and run consolidation.
//! - **Session search** (历史检索): `echo.agent/session/search` over EchoAgent's FTS5.
//! - **Rewind** (回溯): `echo.agent/rewind/{execute,points}`.
//! - **Prompt history** (命令面板): `echo.agent/prompt_history`.
//! - **Slash commands** ("/ 调用技能与指令"): `echo.agent/commands/list`.
//! - **Session fork/info/close**: `echo.agent/session/{fork,info,close}`.
//! - **Plan mode**: idempotent ACP `session/set_mode` requests.
//! - **Folder trust**: `echo.agent/folder_trust/request` responses.
//! - **Subagent / task observation**: `echo.agent/{subagent,task}/*`.
//!
//! All ACP calls go through `ext::call_ext` / `call_ext_value`. File-backed
//! reads (memory markdown) go through direct fs (EchoAgent doesn't expose list).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use echo_agent_runtime::session::memory::{
    init_sqlite_vec, storage::normalize_memory_content, MemoryIndex, MemoryScope, MemoryStorage,
};
use echo_agent_runtime::session::RewindMode as RuntimeRewindMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, State};

use crate::bridge::{FolderTrustOutcome, FolderTrusts};
use crate::commands::AppState;
use crate::ext::{call_ext, raw_params};
use crate::shell_fs::FilesystemAccess;

// ========================================================================
// Memory (资料库)
// ========================================================================

const MEMORY_FILE: &str = "MEMORY.md";
const MAX_MEMORY_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MEMORY_SCAN_ENTRIES: usize = 4_096;
const MAX_MEMORY_RESULTS: usize = 512;
const MAX_MEMORY_RESULT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ADMIN_ID_CHARS: usize = 256;
const MAX_REWRITE_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_ADMIN_ACTION_STRING_BYTES: usize = 64 * 1024;
const MAX_ADMIN_ACTION_TOTAL_BYTES: usize = 256 * 1024;
const MAX_ADMIN_ACTION_NODES: usize = 2_048;
const MAX_ADMIN_RESPONSE_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_ADMIN_RESPONSE_STRING_BYTES: usize = 256 * 1024;
const MAX_ADMIN_RESPONSE_NODES: usize = 100_000;
const MAX_REWIND_PROMPT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ADMIN_RESULTS: usize = 2_000;
const MAX_ADMIN_LISTED_SOURCES: usize = 256;
const MAX_ADMIN_LISTED_PLUGINS: usize = 4_096;
const MAX_LOCAL_PATH_CHARS: usize = 4_096;
const MAX_REMOTE_SOURCE_CHARS: usize = 2_048;

#[derive(Default)]
struct AdminCapabilities {
    plugin_ids: HashSet<String>,
    plugin_roots: HashSet<PathBuf>,
    marketplace_sources: HashSet<String>,
    marketplace_plugins: HashSet<(String, String)>,
}

fn admin_capabilities() -> &'static Mutex<AdminCapabilities> {
    static CAPABILITIES: OnceLock<Mutex<AdminCapabilities>> = OnceLock::new();
    CAPABILITIES.get_or_init(|| Mutex::new(AdminCapabilities::default()))
}

pub(crate) fn clear_runtime_capabilities() {
    *admin_capabilities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = AdminCapabilities::default();
}

fn memory_mutation_lock() -> &'static Mutex<()> {
    echo_agent_runtime::session::memory::storage::memory_mutation_lock()
}

fn valid_admin_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_ADMIN_ID_CHARS
        && !value.chars().any(char::is_control)
}

fn bounded_text(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn require_live_session(state: &AppState, session_id: &str) -> Result<PathBuf, String> {
    if !valid_admin_id(session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    state.session_workspace(session_id)
}

fn validate_json_budget(
    value: &serde_json::Value,
    remaining_bytes: &mut usize,
    remaining_nodes: &mut usize,
    max_string_bytes: usize,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 || *remaining_nodes == 0 {
        return Err("IPC JSON 结构过于复杂".into());
    }
    *remaining_nodes -= 1;
    match value {
        serde_json::Value::String(value) => {
            if value.len() > max_string_bytes || value.len() > *remaining_bytes {
                return Err("IPC JSON 字符串或总大小超出限制".into());
            }
            *remaining_bytes -= value.len();
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_json_budget(
                    value,
                    remaining_bytes,
                    remaining_nodes,
                    max_string_bytes,
                    depth + 1,
                )?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if key.len() > max_string_bytes || key.len() > *remaining_bytes {
                    return Err("IPC JSON 键或总大小超出限制".into());
                }
                *remaining_bytes -= key.len();
                validate_json_budget(
                    value,
                    remaining_bytes,
                    remaining_nodes,
                    max_string_bytes,
                    depth + 1,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_admin_action(value: &serde_json::Value) -> Result<(), String> {
    let mut remaining_bytes = MAX_ADMIN_ACTION_TOTAL_BYTES;
    let mut remaining_nodes = MAX_ADMIN_ACTION_NODES;
    validate_json_budget(
        value,
        &mut remaining_bytes,
        &mut remaining_nodes,
        MAX_ADMIN_ACTION_STRING_BYTES,
        0,
    )
}

fn validate_admin_response(value: &serde_json::Value) -> Result<(), String> {
    let mut remaining_bytes = MAX_ADMIN_RESPONSE_TOTAL_BYTES;
    let mut remaining_nodes = MAX_ADMIN_RESPONSE_NODES;
    validate_json_budget(
        value,
        &mut remaining_bytes,
        &mut remaining_nodes,
        MAX_ADMIN_RESPONSE_STRING_BYTES,
        0,
    )
    .map_err(|_| "Agent Runtime 返回的管理数据过大".to_string())
}

/// One canonical memory document. Global and workspace `MEMORY.md` files are
/// editable. Per-session summaries can be viewed and deleted, but not edited,
/// because their content is generated by the Runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    /// "global" | "workspace" | "session".
    pub scope: String,
    /// `MEMORY.md` for editable documents; a basename for generated session summaries.
    pub path: String,
    pub content: String,
    pub size: u64,
    /// SHA-256 of the file contents, used for optimistic concurrency control.
    pub revision: String,
    /// Last modified timestamp (RFC 3339), when available.
    pub modified_at: Option<String>,
    pub read_only: bool,
}

fn global_memory_dir() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("memory")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryEntryScope {
    Global,
    Workspace,
    Session,
}

impl MemoryEntryScope {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "global" => Ok(Self::Global),
            "workspace" => Ok(Self::Workspace),
            "session" => Ok(Self::Session),
            _ => Err(format!("unknown memory scope: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Session => "session",
        }
    }

    fn writable(self) -> Result<MemoryScope, String> {
        match self {
            Self::Global => Ok(MemoryScope::Global),
            Self::Workspace => Ok(MemoryScope::Workspace),
            Self::Session => Err("session summaries cannot be edited".into()),
        }
    }
}

fn memory_storage(cwd: Option<&str>) -> Result<MemoryStorage, String> {
    let workspace = cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    if cwd.is_some() && !workspace.is_absolute() {
        return Err("cwd must be an absolute path".into());
    }
    Ok(MemoryStorage::new(
        &workspace,
        Some(global_memory_dir().as_path()),
    ))
}

fn open_memory_index(storage: &MemoryStorage) -> Result<Option<MemoryIndex>, String> {
    let db_path = storage.workspace_dir().join("index.sqlite");
    let metadata = match std::fs::symlink_metadata(&db_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(format!(
                "memory index must be a regular file: {}",
                db_path.display()
            ));
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect {}: {error}", db_path.display())),
    };
    if metadata.len() == 0 {
        return Ok(None);
    }

    init_sqlite_vec();
    MemoryIndex::open_or_create_preserving_dimensions(
        &db_path,
        storage.clone(),
        Default::default(),
        crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
    )
    .map(Some)
    .map_err(|error| format!("open memory index {}: {error}", db_path.display()))
}

fn purge_memory_index_paths(storage: &MemoryStorage, paths: &[PathBuf]) -> Result<usize, String> {
    let Some(mut index) = open_memory_index(storage)? else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for path in paths {
        removed = removed.saturating_add(
            index
                .delete_path(path)
                .map_err(|error| format!("purge memory index for {}: {error}", path.display()))?,
        );
    }
    Ok(removed)
}

/// Global MEMORY.md is copied into every workspace search index. Keep all
/// existing indexes in sync even when a live session disabled file watching.
fn memory_index_paths(
    storage: &MemoryStorage,
    scope: MemoryEntryScope,
) -> Result<Vec<PathBuf>, String> {
    if scope != MemoryEntryScope::Global {
        return Ok(vec![storage.workspace_dir().join("index.sqlite")]);
    }
    let mut paths = Vec::new();
    match std::fs::read_dir(storage.global_dir()) {
        Ok(entries) => {
            for (position, entry) in entries.enumerate() {
                if position >= MAX_MEMORY_SCAN_ENTRIES {
                    return Err("工作区记忆索引数量超过安全处理上限".into());
                }
                let entry = entry.map_err(|error| format!("scan memory indexes: {error}"))?;
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    continue;
                }
                let path = entry.path().join("index.sqlite");
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        paths.push(path)
                    }
                    Ok(_) => {
                        return Err(format!(
                            "memory index must be a regular file: {}",
                            path.display()
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("inspect {}: {error}", path.display())),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("scan memory indexes: {error}")),
    }
    Ok(paths)
}

fn sync_memory_indexes(
    storage: &MemoryStorage,
    scope: MemoryEntryScope,
    target: &Path,
    deleted: bool,
) -> Result<(), String> {
    init_sqlite_vec();
    for db_path in memory_index_paths(storage, scope)? {
        match std::fs::symlink_metadata(&db_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("inspect {}: {error}", db_path.display())),
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(format!(
                    "memory index must be a regular file: {}",
                    db_path.display()
                ));
            }
        }
        let mut index = MemoryIndex::open_or_create_preserving_dimensions(
            &db_path,
            storage.clone(),
            Default::default(),
            crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
        )
        .map_err(|error| format!("open memory index {}: {error}", db_path.display()))?;
        if deleted {
            index
                .delete_path(target)
                .map_err(|error| format!("delete memory index {}: {error}", db_path.display()))?;
        } else {
            index
                .reindex_file(target, scope.as_str())
                .map_err(|error| format!("update memory index {}: {error}", db_path.display()))?;
        }
    }
    Ok(())
}

fn collect_markdown_files(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "session summary directory must be a real directory: {}",
                dir.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {}: {error}", dir.display())),
    }

    for entry in std::fs::read_dir(dir)
        .map_err(|error| format!("read session summary directory {}: {error}", dir.display()))?
    {
        if paths.len() >= MAX_MEMORY_SCAN_ENTRIES {
            return Err("session summary count exceeds the safe cleanup limit".into());
        }
        let entry = entry.map_err(|error| format!("read session summary entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_file() && path.extension().and_then(|value| value.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn session_summary_paths(storage: &MemoryStorage) -> Result<Vec<PathBuf>, String> {
    let sessions = storage.sessions_dir();
    let mut paths = Vec::new();
    collect_markdown_files(&sessions, &mut paths)?;
    collect_markdown_files(&sessions.join("archive"), &mut paths)?;
    collect_markdown_files(&sessions.join(".history"), &mut paths)?;

    // Also include stale indexed sources whose files have already disappeared.
    // Otherwise a partial/manual deletion could leave sensitive text searchable
    // forever with no visible row from which the user can remove it.
    if let Some(index) = open_memory_index(storage)? {
        let indexed = index
            .all_indexed_paths()
            .map_err(|error| format!("read memory index paths: {error}"))?;
        let mut seen = paths.iter().cloned().collect::<HashSet<_>>();
        for raw in indexed {
            let path = PathBuf::from(raw);
            if path.starts_with(&sessions) && seen.insert(path.clone()) {
                if paths.len() >= MAX_MEMORY_SCAN_ENTRIES {
                    return Err("session summary count exceeds the safe cleanup limit".into());
                }
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

fn path_belongs_to_session(path: &Path, session_id: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    if stem.contains(session_id) {
        return true;
    }
    let sid8 = session_id.chars().take(8).collect::<String>();
    if sid8.is_empty() {
        return false;
    }
    let marker = format!("-{sid8}");
    stem.ends_with(&marker) || stem.contains(&format!("{marker}-"))
}

fn remove_session_summary_paths(
    storage: &MemoryStorage,
    paths: Vec<PathBuf>,
) -> Result<usize, String> {
    if paths.is_empty() {
        return Ok(0);
    }
    // Purge search chunks before removing their source. This prevents deleted
    // content from remaining retrievable even if the filesystem watcher is late.
    purge_memory_index_paths(storage, &paths)?;
    let mut removed = 0usize;
    for path in paths {
        reject_symlink(&path)?;
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("delete {}: {error}", path.display())),
        }
    }
    Ok(removed)
}

fn delete_session_memory_artifacts_from_storage(
    storage: &MemoryStorage,
    session_id: &str,
) -> Result<usize, String> {
    let paths = session_summary_paths(storage)?
        .into_iter()
        .filter(|path| path_belongs_to_session(path, session_id))
        .collect();
    remove_session_summary_paths(storage, paths)
}

/// Cascade cleanup used after the source chat session has been deleted.
pub(crate) fn delete_session_memory_artifacts(
    cwd: &Path,
    session_id: &str,
) -> Result<usize, String> {
    let cwd = cwd
        .to_str()
        .ok_or_else(|| "workspace path is not valid UTF-8".to_string())?;
    let storage = memory_storage(Some(cwd))?;
    let _guard = memory_mutation_lock()
        .lock()
        .map_err(|_| "记忆写入锁已损坏".to_string())?;
    delete_session_memory_artifacts_from_storage(&storage, session_id)
}

fn authorized_optional_cwd(
    filesystem: &FilesystemAccess,
    cwd: Option<String>,
) -> Result<Option<String>, String> {
    let Some(raw) = cwd.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    Ok(Some(
        filesystem
            .require_workspace(&raw)?
            .to_string_lossy()
            .into_owned(),
    ))
}

fn resolve_memory_path(
    storage: &MemoryStorage,
    scope: MemoryEntryScope,
    relative: &str,
) -> Result<PathBuf, String> {
    match scope {
        MemoryEntryScope::Global => {
            if relative != MEMORY_FILE {
                return Err("global memory path must be MEMORY.md".into());
            }
            Ok(storage.global_memory_file())
        }
        MemoryEntryScope::Workspace => {
            if relative != MEMORY_FILE {
                return Err("workspace memory path must be MEMORY.md".into());
            }
            Ok(storage.workspace_memory_file())
        }
        MemoryEntryScope::Session => {
            let relative = Path::new(relative);
            if relative.components().count() != 1
                || relative.extension().and_then(|value| value.to_str()) != Some("md")
            {
                return Err("invalid session memory path".into());
            }
            Ok(storage.sessions_dir().join(relative))
        }
    }
}

fn file_revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "memory path must not be a symlink: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

fn ensure_real_memory_dir(path: &Path, create: bool, label: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(format!(
            "{label} must be a real directory: {}",
            path.display()
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path)
                .map_err(|error| format!("create {label} {}: {error}", path.display()))?;
            crate::paths::harden_private_dir(path)?;
            Ok(true)
        }
        Err(error) => Err(format!("inspect {label} {}: {error}", path.display())),
    }
}

fn ensure_memory_hierarchy(
    storage: &MemoryStorage,
    scope: MemoryEntryScope,
    create: bool,
) -> Result<bool, String> {
    if !ensure_real_memory_dir(storage.global_dir(), create, "memory root")? {
        return Ok(false);
    }
    if scope == MemoryEntryScope::Global {
        return Ok(true);
    }
    if !storage.workspace_dir().starts_with(storage.global_dir()) {
        return Err("workspace memory directory escaped the memory root".into());
    }
    if !ensure_real_memory_dir(
        storage.workspace_dir(),
        create,
        "workspace memory directory",
    )? {
        return Ok(false);
    }
    if scope == MemoryEntryScope::Workspace {
        return Ok(true);
    }
    ensure_real_memory_dir(&storage.sessions_dir(), false, "session memory directory")
}

fn read_memory_bytes(path: &Path) -> Result<(Vec<u8>, std::fs::Metadata), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "memory path is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_MEMORY_FILE_BYTES {
        return Err(format!(
            "memory file exceeds {} MiB: {}",
            MAX_MEMORY_FILE_BYTES / 1024 / 1024,
            path.display()
        ));
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_MEMORY_FILE_BYTES {
        return Err(format!(
            "memory path is not a bounded regular file: {}",
            path.display()
        ));
    }

    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(MAX_MEMORY_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_MEMORY_FILE_BYTES {
        return Err(format!(
            "memory file exceeds {} MiB: {}",
            MAX_MEMORY_FILE_BYTES / 1024 / 1024,
            path.display()
        ));
    }
    Ok((bytes, opened_metadata))
}

fn read_entry(
    storage: &MemoryStorage,
    scope: MemoryEntryScope,
    relative: &str,
) -> Result<MemoryEntry, String> {
    if !ensure_memory_hierarchy(storage, scope, false)? {
        return Err("memory directory does not exist".into());
    }
    let path = resolve_memory_path(storage, scope, relative)?;
    let (bytes, metadata) = read_memory_bytes(&path)?;
    let size = bytes.len() as u64;
    let revision = file_revision(&bytes);
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("memory file is not valid UTF-8: {}", path.display()))?;
    let modified_at = metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|time| time.to_rfc3339());
    Ok(MemoryEntry {
        scope: scope.as_str().into(),
        path: relative.into(),
        content,
        size,
        revision,
        modified_at,
        read_only: scope == MemoryEntryScope::Session,
    })
}

fn list_memory(
    storage: &MemoryStorage,
    include_workspace: bool,
) -> Result<Vec<MemoryEntry>, String> {
    let mut entries = Vec::new();
    if ensure_memory_hierarchy(storage, MemoryEntryScope::Global, false)?
        && storage.global_memory_file().exists()
    {
        if let Ok(entry) = read_entry(storage, MemoryEntryScope::Global, MEMORY_FILE) {
            entries.push(entry);
        }
    }
    if !include_workspace {
        return Ok(entries);
    }
    if storage.workspace_memory_file().exists() {
        if let Ok(entry) = read_entry(storage, MemoryEntryScope::Workspace, MEMORY_FILE) {
            entries.push(entry);
        }
    }
    if !ensure_memory_hierarchy(storage, MemoryEntryScope::Workspace, false)? {
        return Ok(entries);
    }
    let sessions_exist = ensure_memory_hierarchy(storage, MemoryEntryScope::Session, false)?;
    let mut session_names = if sessions_exist {
        std::fs::read_dir(storage.sessions_dir())
            .map_err(|error| format!("读取会话记忆目录失败：{error}"))?
            .take(MAX_MEMORY_SCAN_ENTRIES)
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                let path = entry.path();
                if !file_type.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("md")
                {
                    return None;
                }
                path.file_name()?.to_str().map(str::to_owned)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    session_names.sort_by(|left, right| right.cmp(left));
    let mut total_bytes = entries.iter().map(|entry| entry.size).sum::<u64>();
    for name in session_names {
        if entries.len() >= MAX_MEMORY_RESULTS || total_bytes >= MAX_MEMORY_RESULT_BYTES {
            break;
        }
        let Ok(entry) = read_entry(storage, MemoryEntryScope::Session, &name) else {
            continue;
        };
        if total_bytes.saturating_add(entry.size) > MAX_MEMORY_RESULT_BYTES {
            continue;
        }
        total_bytes += entry.size;
        entries.push(entry);
    }
    Ok(entries)
}

#[tauri::command]
pub fn memory_list(
    filesystem: State<'_, FilesystemAccess>,
    cwd: Option<String>,
) -> Result<Vec<MemoryEntry>, String> {
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    let storage = memory_storage(cwd.as_deref())?;
    list_memory(&storage, cwd.is_some())
}

#[tauri::command]
pub fn memory_get(
    filesystem: State<'_, FilesystemAccess>,
    scope: String,
    path: String,
    cwd: Option<String>,
) -> Result<String, String> {
    let scope = MemoryEntryScope::parse(&scope)?;
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    if scope != MemoryEntryScope::Global && cwd.is_none() {
        return Err("cwd required for workspace memory".into());
    }
    Ok(read_entry(&memory_storage(cwd.as_deref())?, scope, &path)?.content)
}

fn check_expected_revision(path: &Path, expected: Option<&str>) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected == Some("") => {
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("记忆文件已被删除，请刷新后重试".into());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
        Ok(_) => {}
    }
    let (bytes, _) = read_memory_bytes(path)?;
    match expected {
        Some(value) if file_revision(&bytes) == value => Ok(()),
        _ => Err("记忆已被其他会话修改，请刷新后重试".into()),
    }
}

#[tauri::command]
pub fn memory_save(
    filesystem: State<'_, FilesystemAccess>,
    scope: String,
    path: String,
    content: String,
    cwd: Option<String>,
    expected_revision: Option<String>,
) -> Result<MemoryEntry, String> {
    if content.len() as u64 > MAX_MEMORY_FILE_BYTES {
        return Err(format!(
            "memory content exceeds {} MiB",
            MAX_MEMORY_FILE_BYTES / 1024 / 1024
        ));
    }
    let scope = MemoryEntryScope::parse(&scope)?;
    scope.writable()?;
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    if scope == MemoryEntryScope::Workspace && cwd.is_none() {
        return Err("cwd required for workspace memory".into());
    }
    let storage = memory_storage(cwd.as_deref())?;
    let target = resolve_memory_path(&storage, scope, &path)?;
    let _guard = memory_mutation_lock()
        .lock()
        .map_err(|_| "记忆写入锁已损坏".to_string())?;
    ensure_memory_hierarchy(&storage, scope, true)?;
    reject_symlink(&target)?;
    check_expected_revision(&target, expected_revision.as_deref())?;
    crate::paths::write_private_file(&target, content.as_bytes())?;
    sync_memory_indexes(&storage, scope, &target, false)
        .map_err(|error| format!("记忆文件已保存，但检索索引更新失败：{error}"))?;
    read_entry(&storage, scope, &path)
}

/// Append one normalized note to a canonical memory file. This is the write
/// primitive used by the editor's "new memory" action and `/remember`.
#[tauri::command]
pub fn memory_append(
    filesystem: State<'_, FilesystemAccess>,
    scope: String,
    content: String,
    cwd: Option<String>,
) -> Result<MemoryEntry, String> {
    if content.trim().is_empty() {
        return Err("memory content cannot be empty".into());
    }
    if content.len() as u64 > MAX_MEMORY_FILE_BYTES {
        return Err(format!(
            "memory content exceeds {} MiB",
            MAX_MEMORY_FILE_BYTES / 1024 / 1024
        ));
    }
    let scope = MemoryEntryScope::parse(&scope)?;
    scope.writable()?;
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    if scope == MemoryEntryScope::Workspace && cwd.is_none() {
        return Err("cwd required for workspace memory".into());
    }
    let storage = memory_storage(cwd.as_deref())?;
    let target = resolve_memory_path(&storage, scope, MEMORY_FILE)?;
    if scope == MemoryEntryScope::Workspace && storage.is_ephemeral() {
        return Err("临时工作区不持久化工作区记忆".into());
    }
    let _guard = memory_mutation_lock()
        .lock()
        .map_err(|_| "记忆写入锁已损坏".to_string())?;
    ensure_memory_hierarchy(&storage, scope, true)?;
    reject_symlink(&target)?;
    let normalized = normalize_memory_content(&content);
    let mut combined = match std::fs::symlink_metadata(&target) {
        Ok(_) => read_memory_bytes(&target)?.0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("inspect {}: {error}", target.display())),
    };
    let separator = if combined.is_empty() { "" } else { "\n\n" };
    if combined
        .len()
        .saturating_add(separator.len())
        .saturating_add(normalized.len())
        > MAX_MEMORY_FILE_BYTES as usize
    {
        return Err(format!(
            "memory file would exceed {} MiB",
            MAX_MEMORY_FILE_BYTES / 1024 / 1024
        ));
    }
    combined.extend_from_slice(separator.as_bytes());
    combined.extend_from_slice(normalized.as_bytes());
    crate::paths::write_private_file(&target, &combined)?;
    sync_memory_indexes(&storage, scope, &target, false)
        .map_err(|error| format!("记忆文件已保存，但检索索引更新失败：{error}"))?;
    read_entry(&storage, scope, MEMORY_FILE)
}

#[tauri::command]
pub fn memory_delete(
    filesystem: State<'_, FilesystemAccess>,
    scope: String,
    path: String,
    cwd: Option<String>,
    expected_revision: Option<String>,
) -> Result<(), String> {
    let scope = MemoryEntryScope::parse(&scope)?;
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    if scope != MemoryEntryScope::Global && cwd.is_none() {
        return Err("cwd required for workspace or session memory".into());
    }
    let storage = memory_storage(cwd.as_deref())?;
    let target = resolve_memory_path(&storage, scope, &path)?;
    let _guard = memory_mutation_lock()
        .lock()
        .map_err(|_| "记忆写入锁已损坏".to_string())?;
    if !ensure_memory_hierarchy(&storage, scope, false)? {
        return Err("记忆目录不存在".into());
    }
    reject_symlink(&target)?;
    check_expected_revision(&target, expected_revision.as_deref())?;
    sync_memory_indexes(&storage, scope, &target, true)?;
    std::fs::remove_file(&target).map_err(|error| format!("delete {}: {error}", target.display()))
}

/// Delete every generated session summary for the selected workspace,
/// including already-consolidated archives and atomic-write history copies.
#[tauri::command]
pub fn memory_clear_session_summaries(
    filesystem: State<'_, FilesystemAccess>,
    cwd: String,
) -> Result<usize, String> {
    let cwd = authorized_optional_cwd(&filesystem, Some(cwd))?
        .ok_or_else(|| "cwd required for session memory".to_string())?;
    let storage = memory_storage(Some(&cwd))?;
    let _guard = memory_mutation_lock()
        .lock()
        .map_err(|_| "记忆写入锁已损坏".to_string())?;
    let paths = session_summary_paths(&storage)?;
    remove_session_summary_paths(&storage, paths)
}

/// Rewrite one editor buffer via the active session's model. The Runtime only
/// returns rewritten text; the caller explicitly reviews and saves it.
#[tauri::command]
pub async fn memory_rewrite(
    state: State<'_, AppState>,
    session_id: String,
    raw_text: String,
    context_summary: String,
) -> Result<String, String> {
    require_live_session(&state, &session_id)?;
    if raw_text.len() as u64 > MAX_MEMORY_FILE_BYTES {
        return Err("待改写内容不能超过 2 MiB".into());
    }
    if context_summary.len() > MAX_REWRITE_CONTEXT_BYTES {
        return Err("改写上下文不能超过 256 KiB".into());
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({
        "sessionId": session_id,
        "rawText": raw_text,
        "contextSummary": context_summary,
    }));
    let value: serde_json::Value = call_ext(&tx, "echo.agent/memory/rewrite", params)
        .await
        .map_err(|e| e.to_string())?;
    let rewritten = value
        .get("rewritten")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "memory rewrite response missing rewritten text".to_string())?;
    if rewritten.len() as u64 > MAX_MEMORY_FILE_BYTES {
        return Err("Agent Runtime 返回的改写内容超过 2 MiB".into());
    }
    Ok(rewritten)
}

/// Flush in-flight memory writes to disk (`echo.agent/memory/flush`).
#[tauri::command]
pub async fn memory_flush(state: State<'_, AppState>, session_id: String) -> Result<bool, String> {
    require_live_session(&state, &session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    // The upstream flush request predates the camelCase rewrite request and
    // intentionally deserializes this one field as snake_case.
    let params = raw_params(&serde_json::json!({ "session_id": session_id }));
    let value: serde_json::Value = call_ext(&tx, "echo.agent/memory/flush", params)
        .await
        .map_err(|e| e.to_string())?;
    value
        .get("completed")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "Agent Runtime 未返回记忆提取结果".to_string())
}

/// Run manual Runtime consolidation for the active session. This bypasses
/// periodic gates while returning the actual outcome and preserving Runtime
/// locking, indexing, notifications and failure handling.
#[tauri::command]
pub async fn memory_dream(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, String> {
    require_live_session(&state, &session_id)?;
    crate::commands::require_runtime_ready(Some(&app), &state, None)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({ "session_id": session_id }));
    let value: serde_json::Value = call_ext(&tx, "echo.agent/memory/dream", params)
        .await
        .map_err(|error| error.to_string())?;
    value
        .get("changed")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "Agent Runtime 未返回记忆整理结果".to_string())
}

// ========================================================================
// Session search (FTS5)
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// Snippet of matched content (FTS5 highlights).
    pub snippet: Option<String>,
    /// Match rank (lower = better).
    pub rank: Option<f64>,
    pub updated_at: Option<String>,
}

/// `echo.agent/session/search` response shape (defensive — varies by EchoAgent version).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SearchResponse {
    Results {
        #[serde(default, alias = "results")]
        results: Vec<RawSearchHit>,
    },
    Hits(Vec<RawSearchHit>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSearchHit {
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, alias = "summary")]
    title: Option<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default, alias = "score")]
    rank: Option<f64>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// Full-text search across all sessions (EchoAgent's SQLite FTS5 index).
/// `cwd` optionally narrows to one workspace.
#[tauri::command]
pub async fn session_search(
    filesystem: State<'_, FilesystemAccess>,
    state: State<'_, AppState>,
    query: String,
    cwd: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<SearchHit>, String> {
    if query.chars().count() > 4_096 {
        return Err("搜索关键词过长".into());
    }
    let cwd = authorized_optional_cwd(&filesystem, cwd)?;
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let payload = serde_json::json!({
        "query": query,
        "cwd": cwd,
        "limit": limit,
        "offset": 0,
        "includeContent": true,
    });
    let params = raw_params(&payload);
    let resp: SearchResponse = call_ext(&tx, "echo.agent/session/search", params)
        .await
        .map_err(|e| e.to_string())?;
    let raw = match resp {
        SearchResponse::Results { results } => results,
        SearchResponse::Hits(v) => v,
    };
    Ok(raw
        .into_iter()
        .take(limit as usize)
        .filter(|hit| valid_admin_id(&hit.session_id))
        .map(|h| SearchHit {
            session_id: h.session_id,
            cwd: h.cwd.map(|value| bounded_text(value, MAX_LOCAL_PATH_CHARS)),
            title: h.title.map(|value| bounded_text(value, 1_024)),
            snippet: h.snippet.map(|value| bounded_text(value, 64 * 1024)),
            rank: h.rank,
            updated_at: h.updated_at.map(|value| bounded_text(value, 128)),
        })
        .collect())
}

// ========================================================================
// Rewind (回溯到指定 prompt 索引)
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindPoint {
    #[serde(alias = "prompt_index")]
    pub prompt_index: u32,
    #[serde(default, alias = "prompt_preview")]
    pub prompt_preview: Option<String>,
    #[serde(default, alias = "created_at", alias = "createdAt")]
    pub timestamp: Option<String>,
    /// First assistant response snippet (for timeline display).
    #[serde(
        default,
        alias = "message_preview",
        skip_serializing_if = "Option::is_none"
    )]
    pub message_preview: Option<String>,
    /// Whether this prompt produced file changes (for badge display).
    #[serde(default, alias = "has_file_changes")]
    pub has_file_changes: bool,
    /// Whether this prompt produced memory writes (for badge display).
    #[serde(default, alias = "has_memory_changes")]
    pub has_memory_changes: bool,
    /// Tool calls made during this turn (for timeline detail).
    #[serde(default, alias = "tool_names", skip_serializing_if = "Option::is_none")]
    pub tool_names: Option<Vec<String>>,
}

fn rewind_point_values(value: &serde_json::Value) -> Result<&[serde_json::Value], String> {
    let values = value
        .get("points")
        .or_else(|| value.get("rewindPoints"))
        // Runtime's canonical Rust response uses snake_case.
        .or_else(|| value.get("rewind_points"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .or_else(|| value.as_array().map(Vec::as_slice));
    values.ok_or_else(|| "Agent Runtime 返回了无效的可恢复位置列表".to_string())
}

/// List the prompts a session can rewind to.
#[tauri::command]
pub async fn rewind_points(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<RewindPoint>, String> {
    require_live_session(&state, &session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({ "sessionId": session_id }));
    let v: serde_json::Value = call_ext(&tx, "echo.agent/rewind/points", params)
        .await
        .map_err(|e| e.to_string())?;
    validate_admin_response(&v)?;
    // Response shape: array or { points: [...] }.
    let arr = rewind_point_values(&v)?;
    let points = arr
        .iter()
        .take(MAX_ADMIN_RESULTS)
        .map(|item| {
            serde_json::from_value::<RewindPoint>(item.clone())
                .map_err(|error| format!("Agent Runtime 返回了无效的可恢复位置：{error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(points
        .into_iter()
        .map(|mut point| {
            point.prompt_preview = point
                .prompt_preview
                .map(|value| bounded_text(value, 64 * 1024));
            point.message_preview = point
                .message_preview
                .map(|value| bounded_text(value, 64 * 1024));
            point.timestamp = point.timestamp.map(|value| bounded_text(value, 128));
            point.tool_names = point.tool_names.map(|names| {
                names
                    .into_iter()
                    .take(256)
                    .map(|value| bounded_text(value, 256))
                    .collect()
            });
            point
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindExecution {
    target_prompt_index: u32,
    prompt_text: Option<String>,
}

fn parse_rewind_execution(
    response: serde_json::Value,
    requested_prompt_index: u32,
) -> Result<RewindExecution, String> {
    // `prompt_text` is the exact accepted user prompt and follows the normal
    // 4 MiB send contract. Validate it separately, then apply the tighter
    // generic admin-response budget to the remaining metadata.
    let prompt_value = response
        .get("promptText")
        .or_else(|| response.get("prompt_text"));
    if prompt_value
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.len() > MAX_REWIND_PROMPT_BYTES)
    {
        return Err("Agent Runtime 返回的原始消息过大".into());
    }
    let mut response_metadata = response.clone();
    if let Some(object) = response_metadata.as_object_mut() {
        object.remove("promptText");
        object.remove("prompt_text");
    }
    validate_admin_response(&response_metadata)?;
    match response.get("success").and_then(serde_json::Value::as_bool) {
        Some(true) => {}
        Some(false) => {
            return Err(response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("回溯未成功")
                .to_string());
        }
        None => return Err("Agent Runtime 返回了无效的回溯结果".into()),
    }
    let response_target = response
        .get("targetPromptIndex")
        .or_else(|| response.get("target_prompt_index"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("Agent Runtime 返回的恢复位置无效")?;
    if response_target != requested_prompt_index {
        return Err("Agent Runtime 返回的恢复位置与请求不一致".into());
    }
    let prompt_text = prompt_value
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(RewindExecution {
        target_prompt_index: response_target,
        prompt_text,
    })
}

fn normalize_rewind_mode(mode: Option<&str>) -> Result<RuntimeRewindMode, String> {
    match mode.unwrap_or("all") {
        "all" => Ok(RuntimeRewindMode::All),
        // Accept the retired desktop spellings at the IPC boundary so an old
        // renderer cannot break during an application update. The payload sent
        // to Runtime is always serialized from its own enum below.
        "conversation_only" | "conversation" => Ok(RuntimeRewindMode::ConversationOnly),
        "files_only" | "files" | "code_only" => Ok(RuntimeRewindMode::FilesOnly),
        _ => Err("回溯模式无效".into()),
    }
}

fn rewind_execute_payload(
    session_id: &str,
    target_prompt_index: u32,
    mode: Option<&str>,
    force: bool,
) -> Result<serde_json::Value, String> {
    let mode = normalize_rewind_mode(mode)?;
    Ok(serde_json::json!({
        "sessionId": session_id,
        "targetPromptIndex": target_prompt_index,
        "mode": mode,
        "force": force,
    }))
}

/// Rewind a session to a specific prompt index. Current clients use Runtime's
/// canonical `all` / `conversation_only` / `files_only` wire values; legacy
/// desktop values are normalized at this boundary for update compatibility.
#[tauri::command]
pub async fn rewind_execute(
    state: State<'_, AppState>,
    session_id: String,
    target_prompt_index: u32,
    mode: Option<String>,
    force: Option<bool>,
) -> Result<RewindExecution, String> {
    require_live_session(&state, &session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let payload = rewind_execute_payload(
        &session_id,
        target_prompt_index,
        mode.as_deref(),
        force.unwrap_or(false),
    )?;
    let params = raw_params(&payload);
    let response: serde_json::Value = call_ext(&tx, "echo.agent/rewind/execute", params)
        .await
        .map_err(|e| e.to_string())?;
    parse_rewind_execution(response, target_prompt_index)
}

// ========================================================================
// Session fork / info
// ========================================================================

/// Fork a session: copy its history to a new session id so the user can
/// explore a different direction. Returns the new session id.
#[tauri::command]
pub async fn session_fork(
    state: State<'_, AppState>,
    session_id: String,
    cwd: Option<String>,
) -> Result<String, String> {
    if !valid_admin_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    let _permission_transition_guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;
    let trusted_cwd = state.session_workspace(&session_id)?;
    if let Some(claimed) = cwd.as_deref() {
        let claimed = std::path::PathBuf::from(claimed)
            .canonicalize()
            .map_err(|error| format!("无法解析分叉会话工作区：{error}"))?;
        if claimed != trusted_cwd {
            return Err("分叉会话的工作区与后端会话绑定不一致".into());
        }
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({
        "sessionId": session_id,
        "cwd": trusted_cwd.to_string_lossy(),
    }));
    let v: serde_json::Value = call_ext(&tx, "echo.agent/session/fork", params)
        .await
        .map_err(|e| e.to_string())?;
    // Response: { sessionId: "..." } or bare string.
    let forked_id = v
        .get("sessionId")
        .and_then(|s| s.as_str())
        .or_else(|| v.as_str())
        .ok_or("fork response missing sessionId")?
        .to_string();
    if !valid_admin_id(&forked_id) {
        return Err("Agent Runtime 返回的分叉会话 ID 无效".into());
    }
    // A fork is a continuation of the same task context, so it inherits the
    // source task's permission mode without affecting either task afterwards.
    if crate::policy::locked_permission_mode().is_none() {
        let source_mode = crate::meta::permission_mode(&session_id);
        // Forking creates a persisted continuation, but does not make it a
        // resident Runtime session. If sidecar persistence is unavailable,
        // fail closed to Ask when the fork is loaded.
        if let Err(error) = crate::meta::set_permission_mode(&forked_id, &source_mode) {
            tracing::warn!(%error, %forked_id, "fork created but task permission metadata could not be persisted");
        }
    }
    Ok(forked_id)
}

// ========================================================================
// Slash commands + prompt history
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// Source: "builtin" | "skill" | "plugin".
    #[serde(default)]
    pub source: Option<String>,
}

/// List slash commands EchoAgent knows (builtin + skills + plugins). Powers the
/// Composer's "/" autocomplete.
#[tauri::command]
pub async fn commands_list(
    filesystem: State<'_, FilesystemAccess>,
    state: State<'_, AppState>,
    session_id: Option<String>,
    cwd: Option<String>,
) -> Result<Vec<SlashCommand>, String> {
    let trusted_cwd = match session_id.as_deref() {
        Some(session_id) => {
            if !valid_admin_id(session_id) {
                return Err("会话 ID 无效或过长".into());
            }
            let bound = state.session_workspace(session_id)?;
            if let Some(claimed) = cwd.filter(|value| !value.trim().is_empty()) {
                let canonical = filesystem.require_workspace(&claimed)?;
                if canonical != bound {
                    return Err("命令会话的工作区与后端绑定不一致".into());
                }
            }
            Some(bound.to_string_lossy().into_owned())
        }
        None => authorized_optional_cwd(&filesystem, cwd)?,
    };
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({
        "sessionId": session_id,
        "cwd": trusted_cwd,
    }));
    let v: serde_json::Value = call_ext(&tx, "echo.agent/commands/list", params)
        .await
        .map_err(|e| e.to_string())?;
    validate_admin_response(&v)?;
    Ok(parse_slash_commands(&v))
}

fn parse_slash_commands(v: &serde_json::Value) -> Vec<SlashCommand> {
    let arr = v
        .get("commands")
        .and_then(|c| c.as_array())
        .or_else(|| v.as_array());
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .take(MAX_ADMIN_RESULTS)
        .filter_map(|item| {
            let meta = item.get("_meta");
            let source = meta
                .and_then(|m| m.get("pluginName"))
                .and_then(|s| s.as_str())
                .map(|name| format!("plugin:{name}"))
                .or_else(|| {
                    meta.and_then(|m| m.get("scope"))
                        .and_then(|s| s.as_str())
                        .map(|scope| format!("skill:{scope}"))
                })
                .or_else(|| {
                    meta.and_then(|m| m.get("workflowSource"))
                        .and_then(|s| s.as_str())
                        .map(|source| format!("workflow:{source}"))
                })
                .unwrap_or_else(|| "builtin".to_string());
            Some(SlashCommand {
                name: bounded_text(item.get("name")?.as_str()?.to_string(), 256),
                description: item
                    .get("description")
                    .and_then(|s| s.as_str())
                    .map(|value| bounded_text(value.to_string(), 4_096)),
                argument_hint: item
                    .get("input")
                    .and_then(|input| input.get("hint"))
                    .or_else(|| item.get("argumentHint"))
                    .or_else(|| item.get("argument_hint"))
                    .and_then(|s| s.as_str())
                    .map(|value| bounded_text(value.to_string(), 1_024)),
                source: Some(bounded_text(source, 512)),
            })
        })
        .collect()
}

/// Cross-session prompt history (for the Composer's ↑ history dropdown and
/// the command palette).
#[tauri::command]
pub async fn prompt_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<String>, String> {
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let payload = serde_json::json!({ "limit": limit });
    let params = raw_params(&payload);
    let v: serde_json::Value = call_ext(&tx, "echo.agent/prompt_history", params)
        .await
        .map_err(|e| e.to_string())?;
    validate_admin_response(&v)?;
    // Response: array of strings or { prompts: [...] } or { history: [...] }.
    let arr = v
        .get("prompts")
        .or_else(|| v.get("history"))
        .and_then(|x| x.as_array())
        .or_else(|| v.as_array());
    let Some(arr) = arr else {
        return Ok(Vec::new());
    };
    Ok(arr
        .iter()
        .take(limit as usize)
        .filter_map(|item| {
            item.as_str()
                .map(String::from)
                .or_else(|| item.get("text").and_then(|s| s.as_str()).map(String::from))
        })
        .map(|value| bounded_text(value, 64 * 1024))
        .collect())
}

// ========================================================================
// Subagent / background task observation
// ========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningTask {
    pub id: String,
    pub source: RunningTaskSource,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunningTaskSource {
    Task,
    Subagent,
}

fn parse_running_tasks(
    value: &serde_json::Value,
    requested_session_id: &str,
    source: RunningTaskSource,
) -> Vec<RunningTask> {
    let collection_key = match source {
        RunningTaskSource::Task => "tasks",
        RunningTaskSource::Subagent => "subagents",
    };
    let Some(rows) = value
        .get(collection_key)
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
    else {
        return Vec::new();
    };

    rows.iter()
        .take(MAX_ADMIN_RESULTS)
        .filter_map(|item| {
            // task/list returns terminal history as well as live processes. The
            // floating panel must contain only work the user can still stop.
            if source == RunningTaskSource::Task
                && item.get("completed").and_then(serde_json::Value::as_bool) == Some(true)
            {
                return None;
            }
            let id = item
                .get("id")
                .or_else(|| item.get("taskId"))
                .or_else(|| item.get("task_id"))
                .or_else(|| item.get("subagentId"))
                .or_else(|| item.get("subagent_id"))
                .and_then(|value| value.as_str())?;
            if !valid_admin_id(id) {
                return None;
            }

            let reported_session_id = item
                .get("sessionId")
                .or_else(|| item.get("session_id"))
                .or_else(|| item.get("ownerSessionId"))
                .or_else(|| item.get("owner_session_id"))
                .or_else(|| item.get("parentSessionId"))
                .or_else(|| item.get("parent_session_id"))
                .and_then(serde_json::Value::as_str);
            // Parent and child agents can share a terminal backend. Do not
            // expose another session's process in the active session panel.
            if reported_session_id.is_some_and(|id| id != requested_session_id) {
                return None;
            }

            let kind = match source {
                RunningTaskSource::Task => item.get("kind"),
                RunningTaskSource::Subagent => item
                    .get("subagentType")
                    .or_else(|| item.get("subagent_type")),
            };
            let description = item
                .get("description")
                .or_else(|| item.get("displayCommand"))
                .or_else(|| item.get("display_command"))
                .or_else(|| item.get("command"));
            let status = item
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("running");
            Some(RunningTask {
                id: id.to_string(),
                source,
                kind: kind
                    .and_then(serde_json::Value::as_str)
                    .map(|value| bounded_text(value.to_string(), 256)),
                description: description
                    .and_then(serde_json::Value::as_str)
                    .map(|value| bounded_text(value.to_string(), 4_096)),
                status: Some(bounded_text(status.to_string(), 256)),
                session_id: Some(requested_session_id.to_string()),
            })
        })
        .collect()
}

async fn list_running_tasks(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: &str,
) -> Result<Vec<RunningTask>, String> {
    let params = raw_params(&serde_json::json!({ "sessionId": session_id }));

    // Background terminal tasks and subagents are separate catalogs. Query
    // both; an older runtime may lack one endpoint, so retain the other result.
    let task_result =
        call_ext::<serde_json::Value>(tx, "echo.agent/task/list", params.clone()).await;
    let subagent_result =
        call_ext::<serde_json::Value>(tx, "echo.agent/subagent/list_running", params).await;

    let mut tasks = Vec::new();
    let mut successful_endpoints = 0_u8;
    let mut failures = Vec::new();
    for (method, source, result) in [
        ("echo.agent/task/list", RunningTaskSource::Task, task_result),
        (
            "echo.agent/subagent/list_running",
            RunningTaskSource::Subagent,
            subagent_result,
        ),
    ] {
        match result {
            Ok(value) => match validate_admin_response(&value) {
                Ok(()) => {
                    successful_endpoints += 1;
                    tasks.extend(parse_running_tasks(&value, session_id, source));
                }
                Err(error) => failures.push(format!("{method}: {error}")),
            },
            Err(error) => failures.push(format!("{method}: {error}")),
        }
    }

    if successful_endpoints == 0 {
        return Err(format!("运行中任务查询失败：{}", failures.join("；")));
    }
    for failure in failures {
        tracing::warn!(%failure, "one running-task endpoint was unavailable");
    }

    let mut seen = HashSet::new();
    tasks.retain(|task| seen.insert((task.source, task.id.clone())));
    tasks.truncate(MAX_ADMIN_RESULTS);
    Ok(tasks)
}

/// List running background tasks and subagents owned by one live session.
#[tauri::command]
pub async fn tasks_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<RunningTask>, String> {
    if !valid_admin_id(&session_id) {
        return Err("会话 ID 无效或过长".into());
    }
    // A sidebar selection becomes current just before its Runtime load finishes.
    // It cannot own running work during that short unbound window.
    if state.session_workspace(&session_id).is_err() {
        return Ok(Vec::new());
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    list_running_tasks(&tx, &session_id).await
}

async fn kill_running_task(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: String,
    task_id: String,
    source: RunningTaskSource,
) -> Result<(), String> {
    let response: serde_json::Value = match source {
        RunningTaskSource::Task => {
            let params = raw_params(&serde_json::json!({
                "sessionId": session_id,
                "taskId": task_id,
            }));
            call_ext(tx, "echo.agent/task/kill", params)
                .await
                .map_err(|error| error.to_string())?
        }
        RunningTaskSource::Subagent => {
            let params = raw_params(&serde_json::json!({ "subagentId": task_id }));
            call_ext(tx, "echo.agent/subagent/cancel", params)
                .await
                .map_err(|error| error.to_string())?
        }
    };
    validate_admin_response(&response)?;
    let not_found = response.get("outcome").is_some_and(|outcome| {
        outcome.as_str() == Some("not_found")
            || outcome.get("kind").and_then(serde_json::Value::as_str) == Some("not_found")
    });
    if not_found {
        return Err("任务已结束或不存在".into());
    }
    Ok(())
}

/// Kill a running background task or subagent through its owning endpoint.
#[tauri::command]
pub async fn task_kill(
    state: State<'_, AppState>,
    session_id: String,
    task_id: String,
    source: RunningTaskSource,
) -> Result<(), String> {
    if !valid_admin_id(&task_id) {
        return Err("任务 ID 无效或过长".into());
    }
    require_live_session(&state, &session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    kill_running_task(&tx, session_id, task_id, source).await
}

// ========================================================================
// Folder trust
// ========================================================================

/// Resolve the exact parked `echo.agent/folder_trust/request` reverse request.
/// There is deliberately no client→agent `folder_trust/respond` method: ACP's
/// ExtMethod response channel is the protocol response.
#[tauri::command]
pub async fn folder_trust_respond(
    folder_trusts: State<'_, FolderTrusts>,
    request_id: String,
    trusted: bool,
) -> Result<bool, String> {
    if !valid_admin_id(&request_id) {
        return Err("文件夹信任请求 ID 无效或过长".into());
    }
    let outcome = if trusted {
        FolderTrustOutcome::Trust
    } else {
        FolderTrustOutcome::Reject
    };
    Ok(folder_trusts.resolve(&request_id, outcome).await)
}

// ========================================================================
// Plan mode
// ========================================================================

/// Select the native runtime mode used by EchoAgent. Browser Use and Computer
/// Use install purpose-built prompts while their tools remain isolated in the
/// session-bound, authenticated automation MCP server.
#[tauri::command]
pub async fn set_coding_mode(
    state: State<'_, AppState>,
    session_id: String,
    mode: String,
) -> Result<(), String> {
    require_live_session(&state, &session_id)?;
    let mode_id = match mode.as_str() {
        "ask" => "ask",
        "plan" => "plan",
        "agent" => "default",
        "browser_use" => "browser_use",
        "computer_use" => "computer_use",
        _ => return Err("unsupported coding mode".into()),
    };
    let automation_mode = crate::automation::AutomationMode::parse(mode_id)
        .unwrap_or(crate::automation::AutomationMode::Default);
    crate::automation::validate_mode_capability(automation_mode)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    crate::agent_runtime::set_session_mode_id(&tx, &session_id, mode_id)
        .await
        .map_err(|error| error.to_string())?;
    crate::automation::set_session_mode(&session_id, automation_mode).await?;
    crate::meta::set_agent_mode(&session_id, mode_id)?;
    Ok(())
}

/// Set plan mode idempotently via ACP `session/set_mode`. EchoAgent confirms
/// the authoritative value with `CurrentModeUpdate`.
#[tauri::command]
pub async fn set_plan_mode(
    state: State<'_, AppState>,
    session_id: String,
    enabled: bool,
) -> Result<(), String> {
    require_live_session(&state, &session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    crate::agent_runtime::set_session_mode(&tx, &session_id, enabled)
        .await
        .map_err(|error| error.to_string())?;
    crate::automation::set_session_mode(&session_id, crate::automation::AutomationMode::Default)
        .await?;
    crate::meta::set_agent_mode(&session_id, if enabled { "plan" } else { "default" })?;
    Ok(())
}

/// Backward-compatible command name for older frontend bundles. Its behavior
/// is now a set, not a state-dependent toggle.
#[tauri::command]
pub async fn toggle_plan_mode(
    state: State<'_, AppState>,
    session_id: String,
    enabled: bool,
) -> Result<(), String> {
    set_plan_mode(state, session_id, enabled).await
}

// ========================================================================
// Internal reload (hot-reload config after edits)
// ========================================================================

/// Hot-reload EchoAgent's view of MCP servers / skills / models / config without
/// restarting the app. Maps to `echo.agent/internal/reload_*` extension methods.
/// `kind` ∈ "mcp_all" | "mcp_project" | "skills" | "models".
#[tauri::command]
pub async fn internal_reload(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> Result<(), String> {
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    if kind == "models" {
        return crate::commands::reload_models_and_sync(&app, &state, &tx).await;
    }
    request_internal_reload_and_wait(&tx, &kind).await
}

fn internal_reload_method(kind: &str) -> Result<&'static str, String> {
    match kind {
        "mcp_all" => Ok("echo.agent/internal/reload_all_mcp_servers"),
        "mcp_project" => Ok("echo.agent/internal/reload_project_mcp_servers"),
        "skills" => Ok("echo.agent/internal/reload_skills"),
        "models" => Ok("echo.agent/internal/reload_models"),
        other => Err(format!("unknown reload kind: {other}")),
    }
}

fn internal_reload_request(kind: &str) -> Result<agent_client_protocol::ExtRequest, String> {
    Ok(agent_client_protocol::ExtRequest::new(
        internal_reload_method(kind)?,
        crate::ext::raw_params(&serde_json::json!({})),
    ))
}

/// Send a reload request and do not report success until the runtime has
/// actually applied it. The gateway executes requests concurrently, so merely
/// enqueueing this message does not order a following `session/new` behind it.
async fn request_internal_reload_response(
    tx: &echo_agent_acp::AcpAgentTx,
    kind: &str,
) -> Result<agent_client_protocol::ExtResponse, String> {
    let request = internal_reload_request(kind)?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        echo_agent_acp::acp_send(request, tx),
    )
    .await
    .map_err(|_| format!("reload {kind} timed out after 60 seconds"))?;
    result.map_err(|error| format!("reload {kind}: {error:?}"))
}

pub(crate) async fn request_internal_reload_and_wait(
    tx: &echo_agent_acp::AcpAgentTx,
    kind: &str,
) -> Result<(), String> {
    request_internal_reload_response(tx, kind).await.map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelReloadAck {
    pub(crate) model_count: usize,
    pub(crate) auth_method_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelReloadAckWire {
    models: usize,
    auth_method_id: Option<String>,
    auth_ready: bool,
}

fn parse_model_reload_ack(
    response: agent_client_protocol::ExtResponse,
) -> Result<ModelReloadAck, String> {
    let wire: ModelReloadAckWire = crate::ext::parse_ext_response(&response)
        .map_err(|error| format!("reload models returned an invalid acknowledgement: {error}"))?;
    let auth_method_id = wire
        .auth_method_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    if wire.auth_ready != auth_method_id.is_some() {
        return Err("reload models returned inconsistent authentication state".into());
    }
    Ok(ModelReloadAck {
        model_count: wire.models,
        auth_method_id,
    })
}

/// Reload models and retain the Runtime's authentication acknowledgement.
/// The model catalog and auth method are separate in-memory states; callers
/// must not mark the Runtime ready after observing only the catalog.
pub(crate) async fn request_model_reload_and_wait(
    tx: &echo_agent_acp::AcpAgentTx,
) -> Result<ModelReloadAck, String> {
    let response = request_internal_reload_response(tx, "models").await?;
    parse_model_reload_ack(response)
}

/// Send one of the runtime's internal reload extension requests without requiring
/// a frontend round-trip. Organization Skill synchronization uses this to
/// tighten or expand every resident session immediately after its signed
/// server directory set changes.
pub(crate) fn request_internal_reload(state: &AppState, kind: &str) -> Result<(), String> {
    use echo_agent_acp::{AcpAgentMessage, AcpArgs};
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let request = internal_reload_request(kind)?;
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    let msg = AcpAgentMessage::ExtMethod(AcpArgs {
        request,
        response_tx,
    });
    tx.send(msg).map_err(|e| format!("send reload: {e}"))?;
    Ok(())
}

// ========================================================================
// Plugins + Marketplace (echo.agent/plugins/*, echo.agent/marketplace/*)
// ========================================================================

fn action_object_mut(
    action: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    validate_admin_action(action)?;
    action
        .as_object_mut()
        .ok_or_else(|| "管理操作必须是 JSON 对象".to_string())
}

fn action_kind(object: &serde_json::Map<String, serde_json::Value>) -> Result<&str, String> {
    object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .ok_or_else(|| "管理操作缺少有效 type".to_string())
}

fn ensure_action_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
) -> Result<(), String> {
    if object.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err("管理操作包含未经验证的字段".into())
    }
}

fn required_action_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.chars().count() <= max_chars
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| format!("管理操作字段 {field} 无效或过长"))
}

fn secure_remote_source(value: &str) -> bool {
    if value.len() > MAX_REMOTE_SOURCE_CHARS || value.chars().any(char::is_control) {
        return false;
    }
    if value.starts_with("git@") {
        return !value.chars().any(char::is_whitespace)
            && value
                .split_once(':')
                .is_some_and(|(host, path)| host.len() > 4 && !path.is_empty());
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "https" | "ssh")
        && url.host_str().is_some()
        && url.password().is_none()
        && (url.scheme() == "ssh" || url.username().is_empty())
}

fn normalize_install_source(filesystem: &FilesystemAccess, source: &str) -> Result<String, String> {
    let source = source.trim();
    if secure_remote_source(source) {
        return Ok(source.to_string());
    }
    if source.chars().count() > MAX_LOCAL_PATH_CHARS || source.contains('\0') {
        return Err("插件本地源路径无效或过长".into());
    }
    let canonical = filesystem.require_authorized_package_source(Path::new(source))?;
    if !canonical.is_dir() {
        return Err("插件本地源必须是已授权目录".into());
    }
    Ok(canonical.to_string_lossy().into_owned())
}

fn normalize_plugin_relative_path(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 1_024 || value.chars().any(char::is_control) {
        return None;
    }
    let normalized = value.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./");
    let parts = normalized.split('/').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 32
        || parts
            .iter()
            .any(|part| part.is_empty() || part.contains(':') || matches!(*part, "." | ".."))
    {
        return None;
    }
    Some(parts.join("/"))
}

fn remember_plugins(value: &serde_json::Value) -> Result<(), String> {
    validate_admin_response(value)?;
    let plugins = value
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Agent Runtime 返回的插件列表格式无效".to_string())?;
    if plugins.len() > MAX_ADMIN_LISTED_PLUGINS {
        return Err("Agent Runtime 返回的插件数量过多".into());
    }
    let mut ids = HashSet::new();
    let mut roots = HashSet::new();
    for plugin in plugins {
        if let Some(id) = plugin.get("id").and_then(serde_json::Value::as_str) {
            if !valid_admin_id(id) {
                return Err("Agent Runtime 返回了无效插件 ID".into());
            }
            ids.insert(id.to_string());
        }
        if let Some(root) = plugin.get("root").and_then(serde_json::Value::as_str) {
            if root.chars().count() > MAX_LOCAL_PATH_CHARS || root.contains('\0') {
                return Err("Agent Runtime 返回了无效插件路径".into());
            }
            if let Ok(canonical) = Path::new(root).canonicalize() {
                if canonical.is_dir() {
                    roots.insert(canonical);
                }
            }
        }
    }
    let mut capabilities = admin_capabilities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    capabilities.plugin_ids = ids;
    capabilities.plugin_roots = roots;
    Ok(())
}

fn remember_marketplace(value: &serde_json::Value) -> Result<(), String> {
    validate_admin_response(value)?;
    let sources = value
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Agent Runtime 返回的市场列表格式无效".to_string())?;
    if sources.len() > MAX_ADMIN_LISTED_SOURCES {
        return Err("Agent Runtime 返回的市场源数量过多".into());
    }
    let mut identities = HashSet::new();
    let mut pairs = HashSet::new();
    let mut plugin_count = 0_usize;
    for source in sources {
        let identity = source
            .get("sourceUrlOrPath")
            .and_then(serde_json::Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.chars().count() <= MAX_LOCAL_PATH_CHARS
                    && !value.chars().any(char::is_control)
            })
            .ok_or_else(|| "Agent Runtime 返回了无效市场源".to_string())?;
        identities.insert(identity.to_string());
        let plugins = source
            .get("plugins")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "Agent Runtime 返回了无效市场插件列表".to_string())?;
        plugin_count = plugin_count.saturating_add(plugins.len());
        if plugin_count > MAX_ADMIN_LISTED_PLUGINS {
            return Err("Agent Runtime 返回的市场插件数量过多".into());
        }
        for plugin in plugins {
            let relative = plugin
                .get("relativePath")
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_plugin_relative_path)
                .ok_or_else(|| "Agent Runtime 返回了无效插件相对路径".to_string())?;
            pairs.insert((identity.to_string(), relative));
        }
    }
    let mut capabilities = admin_capabilities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    capabilities.marketplace_sources = identities;
    capabilities.marketplace_plugins = pairs;
    Ok(())
}

fn require_listed_plugin_id(plugin_id: &str) -> Result<(), String> {
    if !valid_admin_id(plugin_id) {
        return Err("插件 ID 无效或过长".into());
    }
    if admin_capabilities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .plugin_ids
        .contains(plugin_id)
    {
        Ok(())
    } else {
        Err("插件未出现在后端最近加载的插件列表中，请刷新后重试".into())
    }
}

fn normalize_plugin_action(
    filesystem: &FilesystemAccess,
    action: &mut serde_json::Value,
) -> Result<(), String> {
    let object = action_object_mut(action)?;
    let kind = action_kind(object)?.to_string();
    match kind.as_str() {
        "reload" => ensure_action_keys(object, &["type"]),
        "install" => {
            ensure_action_keys(object, &["type", "source"])?;
            crate::policy::require_feature("skills")?;
            crate::policy::require_skill_upload()?;
            let source = required_action_string(object, "source", MAX_LOCAL_PATH_CHARS)?;
            let source = normalize_install_source(filesystem, source)?;
            object.insert("source".into(), serde_json::Value::String(source));
            Ok(())
        }
        "uninstall" => {
            ensure_action_keys(object, &["type", "pluginId", "confirmed"])?;
            let plugin_id = required_action_string(object, "pluginId", MAX_ADMIN_ID_CHARS)?;
            require_listed_plugin_id(plugin_id)?;
            if object
                .get("confirmed")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("插件卸载 confirmed 字段无效".into());
            }
            Ok(())
        }
        "update" => {
            ensure_action_keys(object, &["type", "pluginId"])?;
            crate::policy::require_feature("skills")?;
            crate::policy::require_skill_upload()?;
            if let Some(plugin_id) = object.get("pluginId") {
                if !plugin_id.is_null() {
                    let plugin_id = plugin_id
                        .as_str()
                        .ok_or_else(|| "插件 ID 字段无效".to_string())?;
                    require_listed_plugin_id(plugin_id)?;
                }
            }
            Ok(())
        }
        "add" => {
            ensure_action_keys(object, &["type", "path"])?;
            crate::policy::require_feature("skills")?;
            crate::policy::require_skill_upload()?;
            let path = required_action_string(object, "path", MAX_LOCAL_PATH_CHARS)?;
            let path = filesystem.require_authorized_package_source(Path::new(path))?;
            if !path.is_dir() {
                return Err("插件路径必须是已授权目录".into());
            }
            object.insert(
                "path".into(),
                serde_json::Value::String(path.to_string_lossy().into_owned()),
            );
            Ok(())
        }
        "remove" => {
            ensure_action_keys(object, &["type", "path"])?;
            let path = required_action_string(object, "path", MAX_LOCAL_PATH_CHARS)?;
            let canonical = Path::new(path)
                .canonicalize()
                .map_err(|error| format!("无法解析插件路径：{error}"))?;
            let listed = admin_capabilities()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .plugin_roots
                .contains(&canonical);
            if !listed {
                return Err("只能移除后端最近加载的精确插件路径".into());
            }
            object.insert(
                "path".into(),
                serde_json::Value::String(canonical.to_string_lossy().into_owned()),
            );
            Ok(())
        }
        "enable" | "disable" => {
            ensure_action_keys(object, &["type", "pluginId"])?;
            if kind == "enable" {
                crate::policy::require_feature("skills")?;
            }
            let plugin_id = required_action_string(object, "pluginId", MAX_ADMIN_ID_CHARS)?;
            require_listed_plugin_id(plugin_id)
        }
        _ => Err("不支持的插件管理操作".into()),
    }
}

fn require_listed_marketplace_source(source: &str) -> Result<(), String> {
    if admin_capabilities()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .marketplace_sources
        .contains(source)
    {
        Ok(())
    } else {
        Err("市场源未出现在后端最近加载的列表中，请刷新后重试".into())
    }
}

fn normalize_marketplace_action(
    filesystem: &FilesystemAccess,
    action: &mut serde_json::Value,
) -> Result<(), String> {
    let object = action_object_mut(action)?;
    let kind = action_kind(object)?.to_string();
    match kind.as_str() {
        "refresh" => {
            ensure_action_keys(object, &["type", "sourceUrlOrPath"])?;
            crate::policy::require_feature("skills")?;
            crate::policy::require_skill_upload()?;
            if let Some(source) = object.get("sourceUrlOrPath") {
                if !source.is_null() {
                    let source = source
                        .as_str()
                        .ok_or_else(|| "市场源字段无效".to_string())?;
                    require_listed_marketplace_source(source)?;
                }
            }
            Ok(())
        }
        "install" | "update" | "uninstall" => {
            ensure_action_keys(object, &["type", "sourceUrlOrPath", "pluginRelativePath"])?;
            if kind != "uninstall" {
                crate::policy::require_feature("skills")?;
                crate::policy::require_skill_upload()?;
            }
            let source = required_action_string(object, "sourceUrlOrPath", MAX_LOCAL_PATH_CHARS)?;
            let relative = normalize_plugin_relative_path(required_action_string(
                object,
                "pluginRelativePath",
                1_024,
            )?)
            .ok_or_else(|| "市场插件相对路径无效".to_string())?;
            let listed = admin_capabilities()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .marketplace_plugins
                .contains(&(source.to_string(), relative.clone()));
            if !listed {
                return Err("只能操作后端最近加载的市场插件".into());
            }
            object.insert(
                "pluginRelativePath".into(),
                serde_json::Value::String(relative),
            );
            Ok(())
        }
        "add_source" => {
            ensure_action_keys(object, &["type", "url"])?;
            crate::policy::require_feature("skills")?;
            crate::policy::require_skill_upload()?;
            let source = required_action_string(object, "url", MAX_LOCAL_PATH_CHARS)?;
            let source = normalize_install_source(filesystem, source)?;
            object.insert("url".into(), serde_json::Value::String(source));
            Ok(())
        }
        "remove_source" => {
            ensure_action_keys(object, &["type", "sourceUrlOrPath"])?;
            let source = required_action_string(object, "sourceUrlOrPath", MAX_LOCAL_PATH_CHARS)?;
            require_listed_marketplace_source(source)
        }
        _ => Err("不支持的市场管理操作".into()),
    }
}

/// List installed plugins via `echo.agent/plugins/list`. `session_id` is optional —
/// EchoAgent answers from the session's registry when given, otherwise from the
/// shared snapshot. Returns the raw `PluginsListResponse` JSON so the frontend
/// can render the full shape (skill/agent/hook/mcp counts etc.).
#[tauri::command]
pub async fn plugins_list(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    if let Some(session_id) = session_id.as_deref() {
        require_live_session(&state, session_id)?;
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    // EchoAgent requires a session_id; pass an empty string if none — it falls back
    // to the shared snapshot.
    let sid = session_id.unwrap_or_default();
    let params = raw_params(&serde_json::json!({ "sessionId": sid }));
    let value = call_ext(&tx, "echo.agent/plugins/list", params)
        .await
        .map_err(|e| e.to_string())?;
    remember_plugins(&value)?;
    Ok(value)
}

/// Execute a plugin action via `echo.agent/plugins/action`. The frontend supplies
/// the action object verbatim (shape matches EchoAgent's `PluginsActionRequest`).
/// Returns the action's outcome.
#[tauri::command]
pub async fn plugins_action(
    state: State<'_, AppState>,
    filesystem: State<'_, FilesystemAccess>,
    session_id: String,
    mut action: serde_json::Value,
) -> Result<serde_json::Value, String> {
    require_live_session(&state, &session_id)?;
    normalize_plugin_action(&filesystem, &mut action)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let payload = serde_json::json!({ "sessionId": session_id, "action": action });
    let params = raw_params(&payload);
    let value = call_ext(&tx, "echo.agent/plugins/action", params)
        .await
        .map_err(|e| e.to_string())?;
    validate_admin_response(&value)?;
    Ok(value)
}

/// List marketplace sources + their plugins via `echo.agent/marketplace/list`.
/// Returns the raw `MarketplaceListResponse` JSON.
#[tauri::command]
pub async fn marketplace_list(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<serde_json::Value, String> {
    if let Some(session_id) = session_id.as_deref() {
        require_live_session(&state, session_id)?;
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let sid = session_id.unwrap_or_default();
    let params = raw_params(&serde_json::json!({ "sessionId": sid }));
    let value = call_ext(&tx, "echo.agent/marketplace/list", params)
        .await
        .map_err(|e| e.to_string())?;
    remember_marketplace(&value)?;
    Ok(value)
}

/// Execute a marketplace action (install/uninstall/refresh/update/add_source/
/// remove_source). `action` shape matches EchoAgent's `MarketplaceAction` enum.
#[tauri::command]
pub async fn marketplace_action(
    state: State<'_, AppState>,
    filesystem: State<'_, FilesystemAccess>,
    session_id: String,
    mut action: serde_json::Value,
) -> Result<serde_json::Value, String> {
    require_live_session(&state, &session_id)?;
    normalize_marketplace_action(&filesystem, &mut action)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let payload = serde_json::json!({ "sessionId": session_id, "action": action });
    let params = raw_params(&payload);
    let value = call_ext(&tx, "echo.agent/marketplace/action", params)
        .await
        .map_err(|e| e.to_string())?;
    validate_admin_response(&value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{
        check_expected_revision, delete_session_memory_artifacts_from_storage, file_revision,
        init_sqlite_vec, kill_running_task, list_memory, list_running_tasks,
        normalize_plugin_action, parse_model_reload_ack, parse_rewind_execution,
        parse_slash_commands, remember_marketplace, remember_plugins,
        request_internal_reload_and_wait, require_listed_marketplace_source,
        require_listed_plugin_id, resolve_memory_path, rewind_execute_payload, rewind_point_values,
        secure_remote_source, validate_admin_action, MemoryEntryScope, MemoryIndex, MemoryStorage,
        ModelReloadAck, RawSearchHit, RunningTaskSource, MAX_ADMIN_ACTION_STRING_BYTES,
    };

    #[test]
    fn rewind_points_accept_runtime_snake_case_envelope() {
        let value = serde_json::json!({
            "rewind_points": [{ "prompt_index": 0, "prompt_preview": "hello" }]
        });
        let points = rewind_point_values(&value).expect("runtime rewind points");
        assert_eq!(points.len(), 1);
        assert_eq!(points[0]["prompt_index"], 0);

        let point: super::RewindPoint =
            serde_json::from_value(points[0].clone()).expect("snake case rewind point");
        assert_eq!(point.prompt_index, 0);
        assert_eq!(point.prompt_preview.as_deref(), Some("hello"));
    }

    #[test]
    fn rewind_points_reject_unknown_or_malformed_shapes() {
        assert!(rewind_point_values(&serde_json::json!({ "items": [] })).is_err());
        let malformed = serde_json::from_value::<super::RewindPoint>(serde_json::json!({
            "prompt_preview": "missing index"
        }));
        assert!(malformed.is_err());
    }

    #[test]
    fn rewind_execute_payload_uses_runtime_canonical_modes() {
        for (input, expected) in [
            (None, "all"),
            (Some("all"), "all"),
            (Some("conversation_only"), "conversation_only"),
            (Some("files_only"), "files_only"),
            // Compatibility with desktop builds released before the Runtime
            // adopted its canonical snake_case enum values.
            (Some("conversation"), "conversation_only"),
            (Some("files"), "files_only"),
            (Some("code_only"), "files_only"),
        ] {
            let payload =
                rewind_execute_payload("session-1", 7, input, true).expect("supported rewind mode");
            assert_eq!(payload["sessionId"], "session-1");
            assert_eq!(payload["targetPromptIndex"], 7);
            assert_eq!(payload["force"], true);
            assert_eq!(payload["mode"], expected);
        }
    }

    #[test]
    fn rewind_execute_payload_rejects_unknown_mode_before_runtime() {
        let error = rewind_execute_payload("session-1", 0, Some("conversation_v2"), false)
            .expect_err("unknown rewind mode must be rejected");
        assert_eq!(error, "回溯模式无效");
    }

    #[test]
    fn rewind_execute_accepts_exact_snake_case_result() {
        let execution = parse_rewind_execution(
            serde_json::json!({
                "success": true,
                "target_prompt_index": 3,
                "prompt_text": "exact model-facing prompt",
            }),
            3,
        )
        .expect("valid rewind result");

        assert_eq!(execution.target_prompt_index, 3);
        assert_eq!(
            execution.prompt_text.as_deref(),
            Some("exact model-facing prompt")
        );
    }

    #[test]
    fn rewind_execute_rejects_failure_or_wrong_target() {
        let failure = parse_rewind_execution(
            serde_json::json!({
                "success": false,
                "target_prompt_index": 3,
                "error": "rewind rejected",
            }),
            3,
        );
        assert_eq!(failure.unwrap_err(), "rewind rejected");

        let mismatch = parse_rewind_execution(
            serde_json::json!({
                "success": true,
                "target_prompt_index": 4,
                "prompt_text": "wrong turn",
            }),
            3,
        );
        assert_eq!(
            mismatch.unwrap_err(),
            "Agent Runtime 返回的恢复位置与请求不一致"
        );
    }

    #[test]
    fn search_hits_accept_the_current_upstream_summary_and_score_fields() {
        let hit: RawSearchHit = serde_json::from_value(serde_json::json!({
            "sessionId": "session-1",
            "cwd": "/workspace",
            "summary": "真实标题",
            "snippet": "命中内容",
            "score": 0.25,
            "updatedAt": "2026-09-15T08:00:00Z"
        }))
        .unwrap();

        assert_eq!(hit.title.as_deref(), Some("真实标题"));
        assert_eq!(hit.rank, Some(0.25));
    }

    fn assert_session_params(
        arguments: &echo_agent_acp::AcpArgs<agent_client_protocol::ExtRequest>,
        expected: &str,
    ) {
        let params: serde_json::Value =
            serde_json::from_str(arguments.request.params.get()).expect("valid ext params");
        assert_eq!(params["sessionId"], expected);
    }

    #[tokio::test]
    async fn running_tasks_send_session_id_to_both_endpoints_and_merge_live_rows() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move { list_running_tasks(&client.tx, "session-1").await });

        let first = agent.rx.recv().await.expect("task/list request");
        let echo_agent_acp::AcpAgentMessage::ExtMethod(first) = first else {
            panic!("expected task/list ExtMethod");
        };
        assert_eq!(first.request.method.as_ref(), "echo.agent/task/list");
        assert_session_params(&first, "session-1");
        first
            .response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&serde_json::json!({
                    "result": {
                        "tasks": [
                            {
                                "task_id": "task-live",
                                "command": "cargo test",
                                "completed": false,
                                "kind": "bash",
                                "owner_session_id": "session-1"
                            },
                            {
                                "task_id": "task-complete",
                                "command": "done",
                                "completed": true,
                                "owner_session_id": "session-1"
                            },
                            {
                                "task_id": "task-foreign",
                                "command": "other",
                                "completed": false,
                                "owner_session_id": "session-2"
                            }
                        ]
                    }
                })),
            )))
            .expect("task/list response");

        let second = agent
            .rx
            .recv()
            .await
            .expect("subagent/list_running request");
        let echo_agent_acp::AcpAgentMessage::ExtMethod(second) = second else {
            panic!("expected subagent/list_running ExtMethod");
        };
        assert_eq!(
            second.request.method.as_ref(),
            "echo.agent/subagent/list_running"
        );
        assert_session_params(&second, "session-1");
        second
            .response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&serde_json::json!({
                    "result": {
                        "subagents": [{
                            "subagentId": "subagent-live",
                            "parentSessionId": "session-1",
                            "subagentType": "explore",
                            "description": "inspect code"
                        }]
                    }
                })),
            )))
            .expect("subagent/list_running response");

        let rows = task.await.expect("join list task").expect("list succeeds");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "task-live");
        assert_eq!(rows[0].source, RunningTaskSource::Task);
        assert_eq!(rows[0].description.as_deref(), Some("cargo test"));
        assert_eq!(rows[1].id, "subagent-live");
        assert_eq!(rows[1].source, RunningTaskSource::Subagent);
        assert_eq!(rows[1].kind.as_deref(), Some("explore"));
    }

    #[tokio::test]
    async fn running_task_kill_uses_the_owning_endpoint_contract() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            kill_running_task(
                &client.tx,
                "session-1".into(),
                "task-1".into(),
                RunningTaskSource::Task,
            )
            .await
        });

        let message = agent.rx.recv().await.expect("task/kill request");
        let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = message else {
            panic!("expected task/kill ExtMethod");
        };
        assert_eq!(arguments.request.method.as_ref(), "echo.agent/task/kill");
        let params: serde_json::Value =
            serde_json::from_str(arguments.request.params.get()).expect("valid task kill params");
        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["taskId"], "task-1");
        arguments
            .response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&serde_json::json!({
                    "result": { "taskId": "task-1", "outcome": "killed" }
                })),
            )))
            .expect("task/kill response");
        assert_eq!(task.await.expect("join kill task"), Ok(()));

        let (client, mut agent) = echo_agent_acp::acp_channels();
        let subagent = tokio::spawn(async move {
            kill_running_task(
                &client.tx,
                "session-1".into(),
                "subagent-1".into(),
                RunningTaskSource::Subagent,
            )
            .await
        });
        let message = agent.rx.recv().await.expect("subagent/cancel request");
        let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = message else {
            panic!("expected subagent/cancel ExtMethod");
        };
        assert_eq!(
            arguments.request.method.as_ref(),
            "echo.agent/subagent/cancel"
        );
        let params: serde_json::Value = serde_json::from_str(arguments.request.params.get())
            .expect("valid subagent cancel params");
        assert_eq!(params["subagentId"], "subagent-1");
        arguments
            .response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&serde_json::json!({
                    "result": {
                        "subagentId": "subagent-1",
                        "cancelled": true,
                        "outcome": { "kind": "cancelled" }
                    }
                })),
            )))
            .expect("subagent/cancel response");
        assert_eq!(subagent.await.expect("join subagent cancel"), Ok(()));
    }

    #[tokio::test]
    async fn awaited_reload_completes_only_after_runtime_acknowledges_it() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let tx = client.tx;
        let task =
            tokio::spawn(async move { request_internal_reload_and_wait(&tx, "models").await });

        let message = agent.rx.recv().await.expect("reload request");
        assert!(!task.is_finished());
        let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = message else {
            panic!("expected ExtMethod");
        };
        assert_eq!(
            arguments.request.method.as_ref(),
            "echo.agent/internal/reload_models"
        );
        arguments
            .response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&serde_json::json!({ "models": 1 })),
            )))
            .expect("reload response");

        assert_eq!(task.await.expect("reload task"), Ok(()));
    }

    #[tokio::test]
    async fn awaited_reload_rejects_unknown_kinds_without_sending() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let error = request_internal_reload_and_wait(&client.tx, "unknown")
            .await
            .expect_err("unknown kind must fail");

        assert!(error.contains("unknown reload kind"));
        assert!(agent.rx.try_recv().is_err());
    }

    #[test]
    fn model_reload_ack_preserves_runtime_authentication_state() {
        let response =
            agent_client_protocol::ExtResponse::new(crate::ext::raw_params(&serde_json::json!({
                "models": 2,
                "authMethodId": "echoagent.api_key",
                "authReady": true,
            })));

        assert_eq!(
            parse_model_reload_ack(response),
            Ok(ModelReloadAck {
                model_count: 2,
                auth_method_id: Some("echoagent.api_key".into()),
            })
        );
    }

    #[test]
    fn model_reload_ack_rejects_inconsistent_authentication_state() {
        let response =
            agent_client_protocol::ExtResponse::new(crate::ext::raw_params(&serde_json::json!({
                "models": 1,
                "authMethodId": null,
                "authReady": true,
            })));

        assert!(parse_model_reload_ack(response)
            .expect_err("authReady without an auth method must fail closed")
            .contains("inconsistent authentication state"));
    }

    #[test]
    fn memory_list_uses_runtime_layout_and_marks_sessions_read_only() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let storage = MemoryStorage::new(cwd.path(), Some(root.path()));
        std::fs::create_dir_all(storage.sessions_dir()).unwrap();
        std::fs::write(storage.global_memory_file(), "# Global\n").unwrap();
        std::fs::write(storage.workspace_memory_file(), "# Workspace\n").unwrap();
        std::fs::write(
            storage.sessions_dir().join("2026-08-31-test.md"),
            "# Session\n",
        )
        .unwrap();

        let entries = list_memory(&storage, true).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].scope, "global");
        assert_eq!(entries[1].scope, "workspace");
        assert_eq!(entries[2].scope, "session");
        assert!(!entries[0].read_only);
        assert!(entries[2].read_only);
        assert_eq!(entries[0].revision, file_revision(b"# Global\n"));
    }

    #[test]
    fn global_memory_edits_and_deletes_update_every_workspace_index() {
        let root = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_storage = MemoryStorage::new(first.path(), Some(root.path()));
        let second_storage = MemoryStorage::new(second.path(), Some(root.path()));
        let global_file = first_storage.global_memory_file();
        std::fs::write(&global_file, "## Preferences\n\nOldPreferenceToken").unwrap();

        init_sqlite_vec();
        for storage in [&first_storage, &second_storage] {
            std::fs::create_dir_all(storage.workspace_dir()).unwrap();
            let mut index = MemoryIndex::open_or_create(
                &storage.workspace_dir().join("index.sqlite"),
                storage.clone(),
                Default::default(),
                crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
            )
            .unwrap();
            index.reindex_file(&global_file, "global").unwrap();
        }

        std::fs::write(&global_file, "## Preferences\n\nNewPreferenceToken").unwrap();
        super::sync_memory_indexes(
            &first_storage,
            MemoryEntryScope::Global,
            &global_file,
            false,
        )
        .unwrap();
        for storage in [&first_storage, &second_storage] {
            let index = MemoryIndex::open_or_create(
                &storage.workspace_dir().join("index.sqlite"),
                storage.clone(),
                Default::default(),
                crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
            )
            .unwrap();
            assert_eq!(index.search_fts("NewPreferenceToken", 10).unwrap().len(), 1);
            assert!(index
                .search_fts("OldPreferenceToken", 10)
                .unwrap()
                .is_empty());
        }
        super::sync_memory_indexes(&first_storage, MemoryEntryScope::Global, &global_file, true)
            .unwrap();
        let index = MemoryIndex::open_or_create(
            &second_storage.workspace_dir().join("index.sqlite"),
            second_storage,
            Default::default(),
            crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
        )
        .unwrap();
        assert!(index
            .search_fts("NewPreferenceToken", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn memory_paths_reject_arbitrary_and_traversal_paths() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let storage = MemoryStorage::new(cwd.path(), Some(root.path()));

        assert!(resolve_memory_path(&storage, MemoryEntryScope::Global, "notes.md").is_err());
        assert!(resolve_memory_path(&storage, MemoryEntryScope::Session, "../MEMORY.md").is_err());
        assert!(MemoryEntryScope::parse("unexpected").is_err());
    }

    #[test]
    fn deleting_a_source_session_cascades_to_summaries_archives_and_index() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let storage = MemoryStorage::new(cwd.path(), Some(root.path()));
        let sessions = storage.sessions_dir();
        let archive = sessions.join("archive");
        let history = sessions.join(".history");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::create_dir_all(&history).unwrap();

        let direct = sessions.join("2026-09-06-task-01a074eb.md");
        let archived = archive.join("2026-09-06-task-01a074eb-123.md");
        let historical = history.join("2026-09-06-task-01a074eb-456-1.md");
        let stale = sessions.join("2026-09-05-task-01a074eb.md");
        let unrelated = sessions.join("2026-09-06-other-deadbeef.md");
        for path in [&direct, &archived, &historical, &stale, &unrelated] {
            std::fs::write(path, "## Decision\n\nKeep this indexed.").unwrap();
        }

        init_sqlite_vec();
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut index = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            Default::default(),
            crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
        )
        .unwrap();
        index.reindex_file(&direct, "session").unwrap();
        index.reindex_file(&stale, "session").unwrap();
        index.reindex_file(&unrelated, "session").unwrap();
        std::fs::remove_file(&stale).unwrap();
        drop(index);

        let removed = delete_session_memory_artifacts_from_storage(
            &storage,
            "01a074eb-59eb-79e2-bd39-69347c96906a",
        )
        .unwrap();
        assert_eq!(removed, 3);
        assert!(!direct.exists());
        assert!(!archived.exists());
        assert!(!historical.exists());
        assert!(!stale.exists());
        assert!(unrelated.exists());

        let index = MemoryIndex::open_or_create(
            &db_path,
            storage,
            Default::default(),
            crate::agent_runtime::MEMORY_EMBEDDING_DIMENSIONS,
        )
        .unwrap();
        let indexed = index.all_indexed_paths().unwrap();
        assert!(!indexed.contains(&direct.to_string_lossy().into_owned()));
        assert!(!indexed.contains(&stale.to_string_lossy().into_owned()));
        assert!(indexed.contains(&unrelated.to_string_lossy().into_owned()));
    }

    #[test]
    fn model_reasoning_cannot_be_persisted_as_memory() {
        use echo_agent_runtime::session::memory::{dream, flush};

        let flush_result = flush::process_flush_response(
            "<think>private reasoning with ## headings</think>\nNO_REPLY",
            &Default::default(),
        );
        assert_eq!(flush_result, flush::FlushResult::NothingToStore);

        let accepted = flush::process_flush_response(
            "<think>private reasoning</think>\n## Decision\n\nKeep the public result.",
            &Default::default(),
        );
        assert_eq!(
            accepted,
            flush::FlushResult::Accepted("## Decision\n\nKeep the public result.".to_string())
        );
        assert!(dream::process_dream_response(
            "<reasoning>private ## headings</reasoning>\nNO_REPLY"
        )
        .is_none());
    }

    #[test]
    fn plugin_and_marketplace_actions_require_backend_listed_capabilities() {
        let root = tempfile::tempdir().unwrap();
        let listed_plugin = root.path().join("plugin");
        std::fs::create_dir(&listed_plugin).unwrap();
        remember_plugins(&serde_json::json!({
            "plugins": [{
                "id": "user/12345678/demo",
                "root": listed_plugin,
            }]
        }))
        .unwrap();
        assert!(require_listed_plugin_id("user/12345678/demo").is_ok());
        assert!(require_listed_plugin_id("user/12345678/forged").is_err());

        remember_marketplace(&serde_json::json!({
            "sources": [{
                "sourceUrlOrPath": "https://example.test/catalog.git",
                "plugins": [{ "relativePath": "plugins/demo" }]
            }]
        }))
        .unwrap();
        assert!(require_listed_marketplace_source("https://example.test/catalog.git").is_ok());
        assert!(require_listed_marketplace_source("/private/forged").is_err());

        let access = crate::shell_fs::FilesystemAccess::default();
        let mut remove = serde_json::json!({ "type": "remove", "path": listed_plugin });
        assert!(normalize_plugin_action(&access, &mut remove).is_ok());
        let mut forged = serde_json::json!({ "type": "remove", "path": root.path() });
        assert!(normalize_plugin_action(&access, &mut forged).is_err());
    }

    #[test]
    fn admin_actions_reject_unbounded_json_and_unsafe_remote_sources() {
        let oversized = serde_json::json!({
            "type": "install",
            "source": "x".repeat(MAX_ADMIN_ACTION_STRING_BYTES + 1),
        });
        assert!(validate_admin_action(&oversized).is_err());
        assert!(secure_remote_source("https://example.test/plugins.git"));
        assert!(secure_remote_source("ssh://git@example.test/plugins.git"));
        assert!(!secure_remote_source("http://example.test/plugins.git"));
        assert!(!secure_remote_source("file:///private/etc"));
        assert!(!secure_remote_source(
            "https://token@example.test/plugins.git"
        ));
    }

    #[test]
    fn memory_revision_check_detects_stale_edits_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        std::fs::write(&path, "first").unwrap();
        let revision = file_revision(b"first");
        assert!(check_expected_revision(&path, Some(&revision)).is_ok());
        std::fs::write(&path, "second").unwrap();
        assert!(check_expected_revision(&path, Some(&revision)).is_err());

        let missing = dir.path().join("missing.md");
        assert!(check_expected_revision(&missing, Some("")).is_ok());
        assert!(check_expected_revision(&missing, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn memory_writes_reject_symlinked_canonical_files() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.md");
        let link = dir.path().join("MEMORY.md");
        std::fs::write(&outside, "private").unwrap();
        symlink(&outside, &link).unwrap();

        assert!(super::reject_symlink(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn memory_reads_reject_symlinked_storage_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("memory-link");
        symlink(outside.path(), &link).unwrap();
        let storage = MemoryStorage::new(root.path(), Some(&link));

        assert!(list_memory(&storage, true).is_err());
    }

    #[test]
    fn parses_current_acp_command_shape_and_sources() {
        let value = serde_json::json!({
            "commands": [
                {
                    "name": "compact",
                    "description": "Compress history",
                    "input": { "hint": "what to preserve" }
                },
                {
                    "name": "review",
                    "description": "Review changes",
                    "input": { "hint": "optional scope" },
                    "_meta": { "scope": "project", "path": "/repo/SKILL.md" }
                },
                {
                    "name": "acme:deploy",
                    "description": "Deploy",
                    "_meta": { "scope": "project", "pluginName": "acme" }
                },
                {
                    "name": "release",
                    "description": "Release workflow",
                    "_meta": { "workflowSource": "project" }
                }
            ]
        });

        let commands = parse_slash_commands(&value);
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands[0].argument_hint.as_deref(),
            Some("what to preserve")
        );
        assert_eq!(commands[0].source.as_deref(), Some("builtin"));
        assert_eq!(commands[1].source.as_deref(), Some("skill:project"));
        assert_eq!(commands[2].source.as_deref(), Some("plugin:acme"));
        assert_eq!(commands[3].source.as_deref(), Some("workflow:project"));
    }

    #[test]
    fn accepts_legacy_flat_command_shape_and_skips_invalid_rows() {
        let value = serde_json::json!([
            {
                "name": "legacy",
                "description": "Legacy",
                "argumentHint": "value",
                "source": "ignored-legacy-source"
            },
            { "description": "missing name" }
        ]);

        let commands = parse_slash_commands(&value);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "legacy");
        assert_eq!(commands[0].argument_hint.as_deref(), Some("value"));
        assert_eq!(commands[0].source.as_deref(), Some("builtin"));
    }
}
