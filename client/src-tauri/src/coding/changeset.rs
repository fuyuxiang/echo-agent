//! Per-task change set. Records every file the Agent touched together with the
//! content it had before the task started, which is what makes a one-click
//! task rollback possible without reaching for Git history.
//!
//! Files already dirty at task start are snapshotted byte-for-byte and marked
//! `pre_existing`, so rollback restores the user's exact starting content rather
//! than overwriting it with Git HEAD or silently ignoring Agent changes on top.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::coding_workspace::{git_snapshot, CodingGitFile};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use crate::coding::store;
use crate::coding::task::{self, TaskNodeStatus, TaskPhase};
use crate::shell_fs::FilesystemAccess;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// How this task's rollback checkpoint was established. Git adds HEAD-change
/// protection and commit handoff, while filesystem checkpoints keep the core
/// edit/review/rollback workflow available in any ordinary folder.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BaselineMode {
    Git,
    Filesystem,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// Workspace-relative path, always forward-slashed.
    pub path: String,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    /// Content before this task modified the file; `None` for newly created
    /// files, which are deleted rather than restored on rollback.
    pub baseline_content: Option<String>,
    /// The file already had uncommitted edits before the task started.
    pub pre_existing: bool,
}

/// Renderer-facing change metadata. Rollback contents are deliberately kept
/// out of IPC responses: a filesystem checkpoint can contain hundreds of MiB
/// of source data and the workbench only needs the summary below.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeView {
    pub path: String,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    pub pre_existing: bool,
}

impl From<&FileChange> for FileChangeView {
    fn from(change: &FileChange) -> Self {
        Self {
            path: change.path.clone(),
            kind: change.kind,
            added: change.added,
            removed: change.removed,
            pre_existing: change.pre_existing,
        }
    }
}

/// Exact working-tree state at task start. Bytes are base64 encoded so binary
/// files can be restored without conflating "not UTF-8" with "did not exist".
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BaselineFile {
    pub path: String,
    pub existed: bool,
    pub content_base64: Option<String>,
    pub hash: Option<String>,
    /// Missing only in legacy snapshots, whose original mode is unknown.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    pub task_id: String,
    /// Git is an enhancement, not a task-start requirement. Older persisted
    /// change sets infer their mode from `baseline_head` when this is absent.
    #[serde(default)]
    pub baseline_mode: Option<BaselineMode>,
    /// Paths that were dirty before the task started.
    pub baseline_files: Vec<String>,
    /// Commit the working tree was based on when the task began. A changed HEAD
    /// invalidates automatic rollback/commit rather than guessing across history.
    #[serde(default)]
    pub baseline_head: Option<String>,
    /// Dirty and pre-existing untracked/ignored files are captured at task
    /// start; clean tracked files are added lazily from `baseline_head` the
    /// first time the task changes them.
    #[serde(default)]
    pub baseline_entries: Vec<BaselineFile>,
    pub changes: Vec<FileChange>,
    pub created_at: String,
    /// Paths whose diff the user actually opened. The diff-review quality gate
    /// needs evidence of review, not merely the existence of a change.
    #[serde(default)]
    pub reviewed_files: Vec<String>,
    /// Review evidence is tied to file content and is invalidated on any write.
    #[serde(default)]
    pub reviewed_hashes: BTreeMap<String, String>,
    /// Current content hashes for task changes.
    #[serde(default)]
    pub change_hashes: BTreeMap<String, String>,
    /// Aggregate content revision acknowledged by the latest completed
    /// verification batch. New-format tasks must match it before delivery.
    #[serde(default)]
    pub verified_revision: Option<String>,
    /// Content revision captured immediately before the active verification
    /// batch starts. Completion is rejected if files changed meanwhile.
    #[serde(default)]
    pub verification_started_revision: Option<String>,
    /// Files that exceeded the snapshot bound or could not be read safely.
    #[serde(default)]
    pub rollback_unsafe_files: Vec<String>,
    /// Preserves delivery evidence while preventing duplicate commits.
    #[serde(default)]
    pub committed_hash: Option<String>,
    /// Paths included in that commit. A protected pre-existing file may remain
    /// in the working tree after a partial task commit.
    #[serde(default)]
    pub committed_paths: Vec<String>,
}

/// Compact public projection of a persisted change set. Baseline bytes and
/// internal content hashes never cross the Tauri boundary.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSetView {
    pub task_id: String,
    pub content_revision: String,
    pub baseline_mode: Option<BaselineMode>,
    pub changes: Vec<FileChangeView>,
    pub created_at: String,
    pub reviewed_files: Vec<String>,
    pub rollback_unsafe_files: Vec<String>,
    pub committed_hash: Option<String>,
    pub committed_paths: Vec<String>,
}

impl From<&ChangeSet> for ChangeSetView {
    fn from(set: &ChangeSet) -> Self {
        Self {
            task_id: set.task_id.clone(),
            content_revision: set.content_revision(),
            baseline_mode: set.baseline_mode,
            changes: set.changes.iter().map(FileChangeView::from).collect(),
            created_at: set.created_at.clone(),
            reviewed_files: set.reviewed_files.clone(),
            rollback_unsafe_files: set.rollback_unsafe_files.clone(),
            committed_hash: set.committed_hash.clone(),
            committed_paths: set.committed_paths.clone(),
        }
    }
}

impl ChangeSet {
    pub fn effective_baseline_mode(&self) -> BaselineMode {
        // Persisted change sets created before filesystem checkpoints existed
        // were Git-only. In particular, a failed pre-check may have left both
        // fields empty; treating that legacy state as a filesystem checkpoint
        // would make the current workspace look entirely new and unsafe to
        // roll back. Filesystem mode is therefore enabled only when explicitly
        // persisted by `capture_filesystem_baseline`.
        self.baseline_mode.unwrap_or(BaselineMode::Git)
    }

    pub fn total_added(&self) -> u32 {
        self.changes.iter().map(|change| change.added).sum()
    }

    pub fn total_removed(&self) -> u32 {
        self.changes.iter().map(|change| change.removed).sum()
    }

    pub fn is_pre_existing(&self, path: &str) -> bool {
        self.baseline_files.iter().any(|entry| entry == path)
    }

    #[cfg(test)]
    pub fn is_reviewed(&self, path: &str) -> bool {
        self.change_hashes
            .get(path)
            .zip(self.reviewed_hashes.get(path))
            .is_some_and(|(current, reviewed)| current == reviewed)
    }

    pub fn content_revision(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(
            self.baseline_head
                .as_deref()
                .unwrap_or("filesystem")
                .as_bytes(),
        );
        hasher.update([0]);
        for (path, hash) in &self.change_hashes {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(hash.as_bytes());
            hasher.update([0]);
        }
        format!("{:x}", hasher.finalize())
    }

    pub fn verification_is_current(&self) -> bool {
        // Persisted tasks created before revision binding remain readable and
        // retain their previous delivery semantics. Every newly captured task
        // has an explicit baseline mode and therefore requires the binding.
        if self.baseline_mode.is_none() {
            return true;
        }
        let current = self.content_revision();
        self.verified_revision.as_deref() == Some(current.as_str())
    }
}

fn changeset_path(root: &Path, task_id: &str) -> PathBuf {
    store::task_dir(root, task_id).join("changeset.json")
}

pub fn load(root: &Path, task_id: &str) -> ChangeSet {
    store::read_json(&changeset_path(root, task_id))
        .filter(|set: &ChangeSet| set.task_id == task_id)
        .unwrap_or_else(|| ChangeSet {
            task_id: task_id.to_string(),
            baseline_mode: None,
            baseline_files: Vec::new(),
            baseline_head: None,
            baseline_entries: Vec::new(),
            changes: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            reviewed_files: Vec::new(),
            reviewed_hashes: BTreeMap::new(),
            change_hashes: BTreeMap::new(),
            verified_revision: None,
            verification_started_revision: None,
            rollback_unsafe_files: Vec::new(),
            committed_hash: None,
            committed_paths: Vec::new(),
        })
}

fn save(root: &Path, set: &ChangeSet) -> Result<(), String> {
    store::validate_task_id(&set.task_id)?;
    let path = changeset_path(root, &set.task_id);
    if path.exists() && store::read_json::<ChangeSet>(&path).is_none() {
        return Err(format!(
            "变更记录损坏，已保留原文件，请检查 {}",
            path.display()
        ));
    }
    store::write_json(&path, set)
}

/// Reject paths that try to escape the workspace before any write or delete.
pub(crate) fn resolve_in_workspace(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let normalized = relative.replace('\\', "/");
    if normalized.is_empty() || normalized.contains('\0') {
        return Err(format!("非法的工作区路径：{relative}"));
    }
    let mut safe = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => safe.push(part),
            // Absolute paths, `..`, platform prefixes and ambiguous `.`
            // components must never reach a restore/read operation.
            _ => return Err(format!("非法的工作区路径：{relative}")),
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(format!("非法的工作区路径：{relative}"));
    }
    Ok(root.join(safe))
}

/// Refuse to traverse a symlinked directory. A lexical `root.join(path)` check
/// alone is insufficient because an Agent could replace a parent directory
/// with a link pointing outside the workspace before rollback.
fn ensure_safe_parent_chain(
    root: &Path,
    target: &Path,
    create_missing: bool,
) -> Result<(), String> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "文件路径不在工作区内".to_string())?;
    let mut current = root.to_path_buf();
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let Component::Normal(part) = component else {
            return Err("文件路径包含不安全目录分量".into());
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "{} 是符号链接，为避免越出工作区已停止操作",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("{} 不是目录", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_missing => {
                std::fs::create_dir(&current)
                    .map_err(|error| format!("无法创建目录 {}：{error}", current.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!("无法检查目录 {}：{error}", current.display()));
            }
        }
    }
    Ok(())
}

const MAX_BASELINE_BYTES: usize = 16 * 1024 * 1024;
/// Bound the amount of original file content embedded in a non-Git checkpoint.
/// Every discovered path is still hashed after the budget is exhausted, so an
/// unsafe change is detected and surfaced instead of being mistaken for a new
/// file and deleted during rollback.
const MAX_FILESYSTEM_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILESYSTEM_BASELINE_FILES: usize = 100_000;

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn git_head(root: &Path) -> Result<String, String> {
    let repository = crate::process_supervisor::background_sync_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("无法执行 Git：{error}"))?;
    if !repository.status.success() {
        let detail = String::from_utf8_lossy(&repository.stderr)
            .trim()
            .to_string();
        if detail.contains("not a git repository") {
            return Err(
                "当前工作区不是 Git 仓库。请选择仓库目录，或先初始化 Git 并完成首次提交。".into(),
            );
        }
        return Err(format!(
            "无法检查当前工作区的 Git 状态：{}",
            if detail.is_empty() {
                repository.status.to_string()
            } else {
                detail
            }
        ));
    }
    if String::from_utf8_lossy(&repository.stdout).trim() != "true" {
        return Err(
            "当前工作区不是 Git 仓库。请选择仓库目录，或先初始化 Git 并完成首次提交。".into(),
        );
    }

    let output = crate::process_supervisor::background_sync_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("无法读取 Git HEAD：{error}"))?;
    if !output.status.success() {
        return Err("当前 Git 仓库还没有提交。为了建立可回滚基线，请先至少完成一次提交。".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_workspace_bytes(root: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
    let target = resolve_in_workspace(root, path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取 {path} 的元数据：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{path} 不是可安全快照的普通文件"));
    }
    if metadata.len() > MAX_BASELINE_BYTES as u64 {
        return Err(format!("{path} 超过 16 MB，无法建立安全回滚快照"));
    }
    std::fs::read(&target)
        .map(Some)
        .map_err(|error| format!("读取 {path} 失败：{error}"))
}

#[derive(Debug)]
struct LiveFile {
    hash: String,
    content: Option<Vec<u8>>,
    regular: bool,
}

fn normalized_relative_path(path: &Path) -> Result<String, String> {
    path.components()
        .map(|component| {
            let component = component
                .as_os_str()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("工作区包含无法安全表示的文件名：{}", path.display()))?;
            // The persisted wire format always uses `/`. On Unix a literal
            // backslash is legal inside a file name, but accepting it here
            // would collide with a directory separator during restore.
            if !cfg!(windows) && component.contains('\\') {
                return Err(format!(
                    "文件名包含无法安全回滚的反斜杠：{}",
                    path.display()
                ));
            }
            Ok(component)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

fn normalized_git_path(path: &[u8]) -> Result<String, String> {
    let path = std::str::from_utf8(path)
        .map_err(|_| "Git 仓库包含无法安全表示的文件名，已停止建立回滚基线".to_string())?;
    if !cfg!(windows) && path.contains('\\') {
        return Err(format!("Git 文件名包含无法安全回滚的反斜杠：{path}"));
    }
    Ok(path.replace('\\', "/"))
}

pub(crate) fn content_hash_for_mode(bytes: &[u8], mode: &str) -> String {
    let hash = hash_bytes(bytes);
    if mode == "100755" {
        format!("100755:{hash}")
    } else if mode == "120000" {
        format!("120000:{hash}")
    } else {
        hash
    }
}

fn executable_mode(path: &Path) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
        {
            return "100755";
        }
    }
    "100644"
}

fn baseline_hash_matches(baseline: &BaselineFile, current: Option<&str>) -> bool {
    let current = if baseline.mode.is_none() {
        current.map(|hash| hash.strip_prefix("100755:").unwrap_or(hash))
    } else {
        current
    };
    baseline.hash.as_deref() == current
}

fn live_content_hash(root: &Path, path: &str, bytes: &[u8]) -> String {
    content_hash_for_mode(bytes, executable_mode(&root.join(path)))
}

fn hash_regular_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Read enough live state to compare a file with its checkpoint. Symlinks are
/// hashed without following them; newly-created links can therefore be removed
/// safely, while a changed pre-existing link is flagged as rollback-unsafe.
fn live_file(root: &Path, path: &str) -> Result<LiveFile, String> {
    let target = resolve_in_workspace(root, path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    let metadata = std::fs::symlink_metadata(&target)
        .map_err(|error| format!("无法读取 {path} 的元数据：{error}"))?;
    if metadata.file_type().is_symlink() {
        let destination = std::fs::read_link(&target)
            .map_err(|error| format!("读取符号链接 {path} 失败：{error}"))?;
        let mut tagged = b"symlink\0".to_vec();
        tagged.extend_from_slice(destination.to_string_lossy().as_bytes());
        return Ok(LiveFile {
            hash: hash_bytes(&tagged),
            content: None,
            regular: false,
        });
    }
    if !metadata.is_file() {
        return Err(format!("{path} 不是可追踪的普通文件"));
    }
    if metadata.len() <= MAX_BASELINE_BYTES as u64 {
        let bytes = std::fs::read(&target).map_err(|error| format!("读取 {path} 失败：{error}"))?;
        return Ok(LiveFile {
            hash: live_content_hash(root, path, &bytes),
            content: Some(bytes),
            regular: true,
        });
    }
    Ok(LiveFile {
        hash: {
            let hash = hash_regular_file(&target)?;
            if executable_mode(&target) == "100755" {
                format!("100755:{hash}")
            } else {
                hash
            }
        },
        content: None,
        regular: true,
    })
}

fn workspace_paths(root: &Path) -> Result<Vec<String>, String> {
    // A rollback checkpoint is a safety boundary, not a search index. Do not
    // honor mutable .gitignore/.ignore rules here: if a task changes an ignore
    // rule, a pre-existing ignored file must never be mistaken for a new file
    // and deleted during rollback. Only stable, application-owned exclusions
    // for dependency/build metadata are applied.
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || (entry.file_name() != std::ffi::OsStr::new(".git")
                    && (!entry.file_type().is_dir()
                        || !crate::coding::symbols::HARD_IGNORED_DIRS
                            .iter()
                            .any(|ignored| entry.file_name() == std::ffi::OsStr::new(ignored))))
        });
    let mut files = Vec::new();
    for entry in walker {
        let entry =
            entry.map_err(|error| format!("无法完整扫描工作区，已停止建立回滚检查点：{error}"))?;
        if entry.depth() == 0 || !(entry.file_type().is_file() || entry.file_type().is_symlink()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| format!("文件路径不在工作区内：{}", entry.path().display()))?;
        files.push(normalized_relative_path(relative)?);
        if files.len() > MAX_FILESYSTEM_BASELINE_FILES {
            return Err(format!(
                "当前文件夹包含超过 {} 个可追踪文件。请缩小工作区范围，或使用 Git 管理大型工程。",
                MAX_FILESYSTEM_BASELINE_FILES
            ));
        }
    }
    files.sort();
    Ok(files)
}

fn checkpoint_baseline(
    root: &Path,
    path: &str,
    snapshot_bytes: &mut usize,
) -> Result<BaselineFile, String> {
    let target = resolve_in_workspace(root, path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BaselineFile {
                mode: None,
                path: path.to_string(),
                existed: false,
                content_base64: None,
                hash: None,
            });
        }
        Err(error) => return Err(format!("无法读取 {path} 的元数据：{error}")),
        Ok(_) => {}
    }
    let live = live_file(root, path)?;
    let content = live.content.filter(|bytes| {
        if snapshot_bytes.saturating_add(bytes.len()) > MAX_FILESYSTEM_SNAPSHOT_BYTES {
            return false;
        }
        *snapshot_bytes += bytes.len();
        true
    });
    Ok(BaselineFile {
        mode: Some(
            if std::fs::symlink_metadata(&target)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                "120000".into()
            } else {
                executable_mode(&target).into()
            },
        ),
        path: path.to_string(),
        existed: true,
        content_base64: content.map(|bytes| STANDARD.encode(bytes)),
        hash: Some(live.hash),
    })
}

fn git_tracked_paths(root: &Path) -> Result<BTreeSet<String>, String> {
    let output = crate::process_supervisor::background_sync_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["ls-files", "--cached", "-z"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("无法读取 Git 跟踪文件：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "无法读取 Git 跟踪文件：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(normalized_git_path)
        .collect()
}

fn git_dirty_paths(root: &Path) -> Result<Vec<String>, String> {
    let output = crate::process_supervisor::background_sync_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| format!("无法读取 Git 工作区状态：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "无法读取 Git 工作区状态：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let path = entry
                .get(3..)
                .ok_or_else(|| "Git 返回了无法解析的工作区状态".to_string())?;
            normalized_git_path(path)
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn ensure_checkpoint_still_current(
    root: &Path,
    initial_paths: &[String],
    entries: &[BaselineFile],
) -> Result<(), String> {
    if workspace_paths(root)? != initial_paths {
        return Err("建立检查点期间工作区文件发生了变化，请重试".into());
    }
    for entry in entries {
        let expected = entry.hash.as_deref().unwrap_or("<deleted>");
        if live_hash_for_path(root, &entry.path)? != expected {
            return Err(format!("建立检查点期间 {} 发生了变化，请重试", entry.path));
        }
    }
    Ok(())
}

pub fn capture_baseline(
    root: &Path,
    task_id: &str,
    mut dirty_files: Vec<String>,
) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    if !set.changes.is_empty() {
        return Err("任务已经产生变更，不能重写开始基线".into());
    }
    if set.baseline_mode.is_some()
        || set.baseline_head.is_some()
        || !set.baseline_entries.is_empty()
    {
        return Ok(set);
    }
    let baseline_head = git_head(root)?;
    let tracked_paths = git_tracked_paths(root)?;
    let initial_dirty = git_dirty_paths(root)?;
    dirty_files.extend(initial_dirty.iter().cloned());
    let initial_paths = workspace_paths(root)?;
    // Git deliberately hides ignored files. Record every pre-existing
    // untracked/ignored path as well, otherwise changing .gitignore during a
    // task could make an old file look newly created and rollback would delete
    // user data. Tracked clean files remain lazy and come from HEAD on demand.
    dirty_files.extend(
        initial_paths
            .iter()
            .filter(|path| !tracked_paths.contains(*path))
            .cloned(),
    );
    dirty_files.sort();
    dirty_files.dedup();
    let mut snapshot_bytes = 0_usize;
    let entries = dirty_files
        .iter()
        .map(|path| checkpoint_baseline(root, path, &mut snapshot_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    ensure_checkpoint_still_current(root, &initial_paths, &entries)?;
    if git_head(root)? != baseline_head {
        return Err("建立任务基线期间 Git HEAD 已改变，请重试".into());
    }
    if git_dirty_paths(root)? != initial_dirty {
        return Err("建立任务基线期间 Git 工作区发生了变化，请重试".into());
    }
    set.baseline_head = Some(baseline_head);
    set.baseline_mode = Some(BaselineMode::Git);
    set.baseline_files = dirty_files;
    set.baseline_entries = entries;
    set.reviewed_files.clear();
    set.reviewed_hashes.clear();
    set.change_hashes.clear();
    set.verified_revision = None;
    set.verification_started_revision = None;
    set.rollback_unsafe_files.clear();
    set.committed_hash = None;
    set.committed_paths.clear();
    save(root, &set)?;
    Ok(set)
}

/// Establish an application-owned checkpoint for a folder without Git. This is
/// deliberately a full source-tree manifest because shell commands can modify
/// files without going through the editor's write tool. Stable dependency and
/// build-directory exclusions are applied, but mutable ignore files are not.
pub fn capture_filesystem_baseline(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    if !set.changes.is_empty() {
        return Err("任务已经产生变更，不能重写开始检查点".into());
    }
    if set.baseline_mode.is_some()
        || set.baseline_head.is_some()
        || !set.baseline_entries.is_empty()
    {
        return Ok(set);
    }
    let initial_paths = workspace_paths(root)?;
    let mut snapshot_bytes = 0_usize;
    let mut entries = Vec::new();
    for path in &initial_paths {
        entries.push(checkpoint_baseline(root, path, &mut snapshot_bytes)?);
    }
    ensure_checkpoint_still_current(root, &initial_paths, &entries)?;
    set.baseline_mode = Some(BaselineMode::Filesystem);
    set.baseline_head = None;
    set.baseline_files.clear();
    set.baseline_entries = entries;
    set.reviewed_files.clear();
    set.reviewed_hashes.clear();
    set.change_hashes.clear();
    set.verified_revision = None;
    set.verification_started_revision = None;
    set.rollback_unsafe_files.clear();
    set.committed_hash = None;
    set.committed_paths.clear();
    save(root, &set)?;
    Ok(set)
}

/// Record one file change. Re-recording the same path replaces its counters but
/// keeps the first baseline content, because only that snapshot can restore the
/// file to its pre-task state.
pub fn record_change(root: &Path, task_id: &str, change: FileChange) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    let pre_existing = change.pre_existing || set.is_pre_existing(&change.path);
    if !set
        .baseline_entries
        .iter()
        .any(|entry| entry.path == change.path)
    {
        let baseline_bytes = change.baseline_content.as_deref().map(str::as_bytes);
        set.baseline_entries.push(BaselineFile {
            mode: None,
            path: change.path.clone(),
            existed: baseline_bytes.is_some(),
            content_base64: baseline_bytes.map(|bytes| STANDARD.encode(bytes)),
            hash: baseline_bytes.map(hash_bytes),
        });
    }
    let current_hash = live_hash_for_path(root, &change.path)?;
    set.change_hashes.insert(change.path.clone(), current_hash);
    set.verified_revision = None;
    set.reviewed_hashes.remove(&change.path);
    set.reviewed_files.retain(|path| path != &change.path);
    match set
        .changes
        .iter_mut()
        .find(|entry| entry.path == change.path)
    {
        Some(existing) => {
            existing.kind = change.kind;
            existing.added = change.added;
            existing.removed = change.removed;
            existing.pre_existing = pre_existing;
            if existing.baseline_content.is_none() {
                existing.baseline_content = change.baseline_content;
            }
        }
        None => set.changes.push(FileChange {
            pre_existing,
            ..change
        }),
    }
    save(root, &set)?;
    Ok(set)
}

fn live_hash_for_path(root: &Path, path: &str) -> Result<String, String> {
    let target = resolve_in_workspace(root, path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    let hash = match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "<deleted>".to_string(),
        Err(error) => return Err(format!("无法确认 {path} 的当前内容：{error}")),
        // A baseline file can legitimately be replaced by a directory tree.
        // Represent that topology explicitly so the parent path can be
        // reviewed and race-checked while its task-created leaves are tracked
        // independently.
        Ok(metadata) if metadata.is_dir() => "<directory>".to_string(),
        Ok(_) => live_file(root, path)?.hash,
    };
    Ok(hash)
}

pub fn ensure_changes_current(root: &Path, set: &ChangeSet) -> Result<(), String> {
    for change in &set.changes {
        let expected = set
            .change_hashes
            .get(&change.path)
            .ok_or_else(|| format!("{} 缺少内容版本，已停止操作", change.path))?;
        if &live_hash_for_path(root, &change.path)? != expected {
            return Err(format!(
                "{} 在操作确认后又发生了变化，请刷新并重新审阅",
                change.path
            ));
        }
    }
    Ok(())
}

/// Mark one file's diff as reviewed. The hash-bound mark expires whenever the
/// file changes again.
pub fn mark_reviewed(root: &Path, task_id: &str, path: &str) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    if !set.changes.iter().any(|change| change.path == path) {
        return Err("该文件不在当前任务变更集中".into());
    }
    let hash = set
        .change_hashes
        .get(path)
        .cloned()
        .ok_or_else(|| "无法确认该差异的内容版本".to_string())?;
    let live_hash = live_hash_for_path(root, path)?;
    if live_hash != hash {
        return Err("文件在差异生成后又发生了变化，请重新打开并审阅最新差异".into());
    }
    set.reviewed_hashes.insert(path.to_string(), hash);
    set.reviewed_files = set.reviewed_hashes.keys().cloned().collect();
    save(root, &set)?;
    Ok(set)
}

fn mark_reviewed_if_current(
    root: &Path,
    task_id: &str,
    path: &str,
    expected_hash: &str,
) -> Result<ChangeSet, String> {
    if load(root, task_id)
        .change_hashes
        .get(path)
        .map(String::as_str)
        != Some(expected_hash)
    {
        return Err("文件内容已变化，请重新打开最新差异后再标记已审阅".to_string());
    }
    mark_reviewed(root, task_id, path)
}

async fn current_head(root: &Path) -> Result<String, String> {
    let output = crate::process_supervisor::background_async_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("无法读取 Git HEAD：{error}"))?;
    if !output.status.success() {
        return Err("当前工作区不是可用的 Git 仓库".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn text_line_count(bytes: &[u8]) -> usize {
    std::str::from_utf8(bytes)
        .map(|text| text.lines().count())
        .unwrap_or_default()
}

/// Compute task-baseline-relative line totals in process. Checkpoint diffing is
/// available even when the workspace is not a repository or Git is not
/// installed on the machine.
fn line_delta(baseline: Option<&[u8]>, current: Option<&[u8]>) -> (usize, usize) {
    match (baseline, current) {
        (None, Some(bytes)) => return (text_line_count(bytes), 0),
        (Some(bytes), None) => return (0, text_line_count(bytes)),
        (None, None) => return (0, 0),
        _ => {}
    }
    let Ok(baseline) = std::str::from_utf8(baseline.unwrap_or_default()) else {
        return (0, 0);
    };
    let Ok(current) = std::str::from_utf8(current.unwrap_or_default()) else {
        return (0, 0);
    };
    similar::TextDiff::from_lines(baseline, current)
        .iter_all_changes()
        .fold((0, 0), |(added, removed), change| match change.tag() {
            similar::ChangeTag::Insert => (added + 1, removed),
            similar::ChangeTag::Delete => (added, removed + 1),
            similar::ChangeTag::Equal => (added, removed),
        })
}

async fn read_workspace_bytes_async(root: &Path, path: &str) -> Result<Option<Vec<u8>>, String> {
    let target = resolve_in_workspace(root, path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    let metadata = match tokio::fs::symlink_metadata(&target).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取 {path} 的元数据：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{path} 不是可安全回滚的普通文件"));
    }
    if metadata.len() > MAX_BASELINE_BYTES as u64 {
        return Err(format!("{path} 超过 16 MB 安全快照上限"));
    }
    tokio::fs::read(&target)
        .await
        .map(Some)
        .map_err(|error| format!("读取 {path} 失败：{error}"))
}

async fn baseline_from_git(
    root: &Path,
    head: &str,
    live: &CodingGitFile,
) -> Result<BaselineFile, String> {
    if matches!(live.status.as_str(), "added" | "untracked") {
        return Ok(BaselineFile {
            mode: None,
            path: live.path.clone(),
            existed: false,
            content_base64: None,
            hash: None,
        });
    }
    let spec = format!("{head}:{}", live.path);
    let output = crate::process_supervisor::background_async_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["show", "--no-ext-diff", &spec])
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("读取 {} 的 Git 基线失败：{error}", live.path))?;
    if !output.status.success() {
        return Err(format!("无法从任务基线恢复 {}，已禁止自动回滚", live.path));
    }
    if output.stdout.len() > MAX_BASELINE_BYTES {
        return Err(format!("{} 超过 16 MB 安全快照上限", live.path));
    }
    let mode_output = crate::process_supervisor::background_async_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(crate::coding_workspace::read_only_git_overrides(root)?)
        .args(["ls-tree", "-z", head, "--", &live.path])
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("读取文件模式失败：{error}"))?;
    let mode = String::from_utf8_lossy(&mode_output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("100644")
        .to_string();
    if mode != "100644" && mode != "100755" {
        return Err(format!("{} 的文件类型不支持自动恢复", live.path));
    }
    Ok(BaselineFile {
        mode: Some(mode.clone()),
        path: live.path.clone(),
        existed: true,
        hash: Some(content_hash_for_mode(&output.stdout, &mode)),
        content_base64: Some(STANDARD.encode(output.stdout)),
    })
}

/// Snapshot the live Git state and record every file the Agent (or the user
/// during the task) actually changed, relative to the baseline the user
/// captured when the task started.
///
/// Dirty-at-start files remain included when the task changes their contents;
/// the persisted byte snapshot protects the user's work during rollback.
#[cfg(test)]
pub async fn sync_from_git(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let set = load(root, task_id);
    sync_from_git_with_set(root, set).await
}

async fn sync_from_git_with_set(root: &Path, mut set: ChangeSet) -> Result<ChangeSet, String> {
    let snapshot = git_snapshot(root).await;
    if !snapshot.has_git {
        return Err(
            "该任务使用 Git 基线，但当前工作区已不是 Git 仓库；为避免覆盖文件，已停止同步".into(),
        );
    }
    let head = current_head(root).await?;
    if let Some(committed) = set.committed_hash.clone() {
        if set.baseline_head.as_deref() != Some(committed.as_str()) && head == committed {
            let paths = set.committed_paths.clone();
            let mut partial = BTreeMap::new();
            for path in paths.iter().filter(|path| set.is_pre_existing(path)) {
                let baseline = baseline_for(&set, path)?;
                let original = baseline
                    .content_base64
                    .as_deref()
                    .and_then(|encoded| STANDARD.decode(encoded).ok())
                    .ok_or_else(|| {
                        format!("旧部分提交的 {path} 缺少原始内容；已保留历史，不能自动迁移")
                    })?;
                let old_head = set
                    .baseline_head
                    .as_deref()
                    .ok_or("旧部分提交缺少原始 HEAD")?;
                let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
                let ours = temp.path().join("original-user");
                let ancestor = temp.path().join("old-head");
                let theirs = temp.path().join("committed");
                std::fs::write(&ours, original).map_err(|error| error.to_string())?;
                for (head, target) in [(old_head, &ancestor), (committed.as_str(), &theirs)] {
                    let output = crate::process_supervisor::background_sync_command("git")
                        .env("GIT_OPTIONAL_LOCKS", "0")
                        .args(crate::coding_workspace::read_only_git_overrides(root)?)
                        .args(["show", &format!("{head}:{path}")])
                        .current_dir(root)
                        .output()
                        .map_err(|error| error.to_string())?;
                    if !output.status.success() {
                        return Err(format!("旧部分提交的 {path} Git 对象不可读取，原数据保留"));
                    }
                    std::fs::write(target, output.stdout).map_err(|error| error.to_string())?;
                }
                let merged = crate::process_supervisor::background_sync_command("git")
                    .env("GIT_OPTIONAL_LOCKS", "0")
                    .args(crate::coding_workspace::read_only_git_overrides(root)?)
                    .args(["merge-file", "-p"])
                    .arg(&ours)
                    .arg(&ancestor)
                    .arg(&theirs)
                    .output()
                    .map_err(|error| error.to_string())?;
                if !merged.status.success() {
                    return Err(format!(
                        "旧部分提交的 {path} 迁移发生冲突；原文件与历史均已保留，请手工核对"
                    ));
                }
                partial.insert(
                    path.clone(),
                    String::from_utf8(merged.stdout)
                        .map_err(|_| "旧部分提交不是可迁移文本".to_string())?,
                );
            }
            // Old releases froze tasks after one commit. Advance only their
            // known committed baselines, preserving all unselected snapshots.
            set = mark_committed_with_baselines(root, &set.task_id, &committed, &paths, &partial)?;
        }
    }
    let baseline_head = set
        .baseline_head
        .clone()
        .ok_or_else(|| "任务缺少安全基线，请新建任务后重试".to_string())?;
    if head != baseline_head {
        return Err("任务执行期间 Git HEAD 已改变，为避免覆盖新提交，已停止自动同步".into());
    }

    let mut entries: BTreeMap<String, BaselineFile> = set
        .baseline_entries
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let mut changes = Vec::new();
    let mut hashes = BTreeMap::new();
    let mut unsafe_files = Vec::new();

    for live in &snapshot.files {
        let (current, current_hash, current_exists) =
            match read_workspace_bytes_async(root, &live.path).await {
                Ok(bytes) => {
                    let hash = bytes
                        .as_deref()
                        .map(|bytes| live_content_hash(root, &live.path, bytes));
                    let exists = bytes.is_some();
                    (bytes, hash, exists)
                }
                Err(_) => {
                    // Git reports the deleted parent path when a tracked file is
                    // replaced by a directory. The directory's leaf files are
                    // separate untracked entries and remain independently hashed.
                    // Keep this safe topology distinguishable from unreadable
                    // special files such as symlinks.
                    let hash = live_hash_for_path(root, &live.path).ok();
                    if hash.as_deref() == Some("<directory>") {
                        (None, hash, true)
                    } else {
                        unsafe_files.push(live.path.clone());
                        (None, hash, false)
                    }
                }
            };
        let baseline = if let Some(existing) = entries.get(&live.path) {
            existing.clone()
        } else {
            match baseline_from_git(root, &baseline_head, live).await {
                Ok(entry) => {
                    entries.insert(live.path.clone(), entry.clone());
                    entry
                }
                Err(_) => {
                    unsafe_files.push(live.path.clone());
                    BaselineFile {
                        mode: None,
                        path: live.path.clone(),
                        existed: !matches!(live.status.as_str(), "added" | "untracked"),
                        content_base64: None,
                        hash: None,
                    }
                }
            }
        };
        let unchanged = baseline.existed == current_exists
            && baseline_hash_matches(&baseline, current_hash.as_deref());
        if unchanged {
            continue;
        }
        let hash = current_hash.unwrap_or_else(|| "<deleted>".into());
        hashes.insert(live.path.clone(), hash);
        let baseline_bytes = baseline
            .content_base64
            .as_deref()
            .and_then(|content| STANDARD.decode(content).ok());
        let baseline_content = baseline_bytes
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_string);
        let (added, removed) = line_delta(baseline_bytes.as_deref(), current.as_deref());
        changes.push(FileChange {
            path: live.path.clone(),
            kind: match (baseline.existed, current_exists, live.status.as_str()) {
                (false, true, _) => ChangeKind::Added,
                (true, false, _) => ChangeKind::Deleted,
                (true, true, "renamed") => ChangeKind::Renamed,
                _ => ChangeKind::Modified,
            },
            added: added.min(u32::MAX as usize) as u32,
            removed: removed.min(u32::MAX as usize) as u32,
            baseline_content,
            pre_existing: set.is_pre_existing(&live.path),
        });
    }

    // A pre-existing dirty file can disappear from `git status` if the Agent
    // overwrites it with HEAD, and a pre-existing untracked file can disappear
    // after deletion. Compare every persisted baseline too; otherwise those two
    // destructive outcomes would be invisible and impossible to review/restore.
    let live_paths: BTreeSet<&str> = snapshot
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    for baseline in entries.values() {
        if live_paths.contains(baseline.path.as_str()) {
            continue;
        }
        let (current, current_hash, current_exists) =
            match read_workspace_bytes_async(root, &baseline.path).await {
                Ok(bytes) => {
                    let hash = bytes
                        .as_deref()
                        .map(|bytes| live_content_hash(root, &baseline.path, bytes));
                    let exists = bytes.is_some();
                    (bytes, hash, exists)
                }
                Err(_) => {
                    let hash = live_hash_for_path(root, &baseline.path).ok();
                    if hash.as_deref() == Some("<directory>") {
                        (None, hash, true)
                    } else {
                        unsafe_files.push(baseline.path.clone());
                        (None, hash, false)
                    }
                }
            };
        if baseline.existed == current_exists
            && baseline_hash_matches(baseline, current_hash.as_deref())
        {
            continue;
        }
        hashes.insert(
            baseline.path.clone(),
            current_hash.unwrap_or_else(|| "<deleted>".into()),
        );
        let baseline_bytes = baseline
            .content_base64
            .as_deref()
            .and_then(|content| STANDARD.decode(content).ok());
        let baseline_text = baseline_bytes
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_string);
        let (added, removed) = line_delta(baseline_bytes.as_deref(), current.as_deref());
        changes.push(FileChange {
            path: baseline.path.clone(),
            kind: match (baseline.existed, current_exists) {
                (false, true) => ChangeKind::Added,
                (true, false) => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            },
            added: added.min(u32::MAX as usize) as u32,
            removed: removed.min(u32::MAX as usize) as u32,
            baseline_content: baseline_text,
            pre_existing: set.is_pre_existing(&baseline.path),
        });
    }

    changes.sort_by(|left, right| left.path.cmp(&right.path));
    unsafe_files.sort();
    unsafe_files.dedup();

    let content_changed = set.change_hashes != hashes;
    set.baseline_entries = entries.into_values().collect();
    set.changes = changes;
    set.change_hashes = hashes;
    if content_changed {
        set.verified_revision = None;
    }
    set.rollback_unsafe_files = unsafe_files;
    set.reviewed_hashes.retain(|path, reviewed_hash| {
        set.change_hashes
            .get(path)
            .is_some_and(|current_hash| current_hash == reviewed_hash)
    });
    set.reviewed_files = set.reviewed_hashes.keys().cloned().collect();
    let stored = set.clone();
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || save(&root, &stored))
        .await
        .map_err(|error| format!("保存变更集失败：{error}"))??;
    Ok(set)
}

/// Compare a plain folder with the application-owned task checkpoint. Unlike
/// tool-call bookkeeping, this also catches writes made by shell commands and
/// by the user's editor while the Agent is working.
#[cfg(test)]
pub fn sync_from_filesystem(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let set = load(root, task_id);
    sync_from_filesystem_with_set(root, set)
}

fn sync_from_filesystem_with_set(root: &Path, mut set: ChangeSet) -> Result<ChangeSet, String> {
    if set.effective_baseline_mode() != BaselineMode::Filesystem {
        return Err("当前任务不是本地检查点模式".into());
    }

    let mut entries: BTreeMap<String, BaselineFile> = set
        .baseline_entries
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let current_paths = workspace_paths(root)?;
    let current_path_set: BTreeSet<String> = current_paths.iter().cloned().collect();
    let mut changes = Vec::new();
    let mut hashes = BTreeMap::new();
    let mut unsafe_files = Vec::new();

    for path in current_paths {
        let live = match live_file(root, &path) {
            Ok(live) => live,
            Err(_) => {
                unsafe_files.push(path);
                continue;
            }
        };
        let baseline = entries.get(&path).cloned().unwrap_or_else(|| {
            let baseline = BaselineFile {
                mode: None,
                path: path.clone(),
                existed: false,
                content_base64: None,
                hash: None,
            };
            entries.insert(path.clone(), baseline.clone());
            baseline
        });
        if baseline.existed && baseline_hash_matches(&baseline, Some(live.hash.as_str())) {
            continue;
        }

        if baseline.existed && (baseline.content_base64.is_none() || !live.regular) {
            unsafe_files.push(path.clone());
        }
        hashes.insert(path.clone(), live.hash);
        let baseline_bytes = baseline
            .content_base64
            .as_deref()
            .and_then(|content| STANDARD.decode(content).ok());
        let baseline_content = baseline_bytes
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_string);
        let (added, removed) = match live.content.as_deref() {
            Some(current) => line_delta(baseline_bytes.as_deref(), Some(current)),
            None => (0, 0),
        };
        changes.push(FileChange {
            path,
            kind: if baseline.existed {
                ChangeKind::Modified
            } else {
                ChangeKind::Added
            },
            added: added.min(u32::MAX as usize) as u32,
            removed: removed.min(u32::MAX as usize) as u32,
            baseline_content,
            pre_existing: false,
        });
    }

    // Anything that existed at task start but is absent now was deleted. A
    // content-less baseline (large file or symlink) is reported but cannot be
    // rolled back automatically, so delivery remains safely blocked.
    for baseline in entries.values() {
        if !baseline.existed || current_path_set.contains(&baseline.path) {
            continue;
        }
        if baseline.content_base64.is_none() {
            unsafe_files.push(baseline.path.clone());
        }
        // The old file may have been replaced by a directory. Preserve that
        // distinction in the revision hash; rollback will preflight every leaf
        // before removing the now-empty directory and restoring the file.
        hashes.insert(
            baseline.path.clone(),
            live_hash_for_path(root, &baseline.path)?,
        );
        let baseline_bytes = baseline
            .content_base64
            .as_deref()
            .and_then(|content| STANDARD.decode(content).ok());
        let baseline_content = baseline_bytes
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_string);
        let (added, removed) = line_delta(baseline_bytes.as_deref(), None);
        changes.push(FileChange {
            path: baseline.path.clone(),
            kind: ChangeKind::Deleted,
            added: added.min(u32::MAX as usize) as u32,
            removed: removed.min(u32::MAX as usize) as u32,
            baseline_content,
            pre_existing: false,
        });
    }

    changes.sort_by(|left, right| left.path.cmp(&right.path));
    unsafe_files.sort();
    unsafe_files.dedup();
    let content_changed = set.change_hashes != hashes;
    set.baseline_mode = Some(BaselineMode::Filesystem);
    set.baseline_entries = entries.into_values().collect();
    set.changes = changes;
    set.change_hashes = hashes;
    if content_changed {
        set.verified_revision = None;
    }
    set.rollback_unsafe_files = unsafe_files;
    set.reviewed_hashes.retain(|path, reviewed_hash| {
        set.change_hashes
            .get(path)
            .is_some_and(|current_hash| current_hash == reviewed_hash)
    });
    set.reviewed_files = set.reviewed_hashes.keys().cloned().collect();
    save(root, &set)?;
    Ok(set)
}

pub async fn sync_changes(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let load_root = root.to_path_buf();
    let load_task = task_id.to_string();
    let set = tokio::task::spawn_blocking(move || load(&load_root, &load_task))
        .await
        .map_err(|error| format!("读取任务检查点失败：{error}"))?;
    match set.effective_baseline_mode() {
        BaselineMode::Git => sync_from_git_with_set(root, set).await,
        BaselineMode::Filesystem => {
            let root = root.to_path_buf();
            tokio::task::spawn_blocking(move || sync_from_filesystem_with_set(&root, set))
                .await
                .map_err(|error| format!("同步本地检查点失败：{error}"))?
        }
    }
}

#[tauri::command]
pub async fn coding_changeset_sync(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    let set = sync_changes(&root, &task_id).await?;
    Ok(ChangeSetView::from(&set))
}

fn remove_empty_directory_tree(path: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(path)
        .map_err(|error| format!("无法检查目录 {}：{error}", path.display()))?
    {
        let entry = entry.map_err(|error| format!("无法读取目录 {}：{error}", path.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法检查 {}：{error}", entry.path().display()))?;
        if !file_type.is_dir() {
            return Err(format!(
                "{} 包含未纳入任务变更的文件，已停止回滚",
                path.display()
            ));
        }
        remove_empty_directory_tree(&entry.path())?;
    }
    std::fs::remove_dir(path).map_err(|error| format!("无法移除空目录 {}：{error}", path.display()))
}

fn ensure_replacement_directory_is_task_owned(
    root: &Path,
    directory: &Path,
    set: &ChangeSet,
) -> Result<(), String> {
    for entry in walkdir::WalkDir::new(directory).follow_links(false) {
        let entry = entry.map_err(|error| format!("无法预检替换目录：{error}"))?;
        if entry.depth() == 0 || entry.file_type().is_dir() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| "替换目录包含工作区外路径".to_string())?;
        let relative = normalized_relative_path(relative)?;
        let baseline = baseline_for(set, &relative)?;
        if baseline.existed || !set.changes.iter().any(|change| change.path == relative) {
            return Err(format!(
                "{} 包含任务开始前的文件，已停止回滚",
                directory.display()
            ));
        }
    }
    Ok(())
}

fn restore_one(root: &Path, baseline: &BaselineFile) -> Result<(), String> {
    let target = resolve_in_workspace(root, &baseline.path)?;
    ensure_safe_parent_chain(root, &target, baseline.existed)?;
    if baseline.existed {
        let encoded = baseline
            .content_base64
            .as_deref()
            .ok_or_else(|| format!("{} 缺少可回滚快照", baseline.path))?;
        let content = STANDARD
            .decode(encoded)
            .map_err(|error| format!("解码 {} 的回滚快照失败：{error}", baseline.path))?;
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() => {
                // Added files are removed before original files are restored.
                // Only empty directories may be cleaned up here; any remaining
                // file or symlink makes the operation fail closed.
                remove_empty_directory_tree(&target)?;
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                // Remove the link itself; never follow it while restoring.
                std::fs::remove_file(&target)
                    .map_err(|error| format!("移除 {} 的符号链接失败：{error}", baseline.path))?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查 {}：{error}", baseline.path)),
        }
        std::fs::write(&target, content)
            .map_err(|error| format!("还原 {} 失败：{error}", baseline.path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&target)
                .map_err(|error| error.to_string())?
                .permissions();
            let mode = permissions.mode();
            if let Some(captured_mode) = baseline.mode.as_deref() {
                permissions.set_mode(if captured_mode == "100755" {
                    mode | 0o111
                } else {
                    mode & !0o111
                });
            }
            std::fs::set_permissions(&target, permissions)
                .map_err(|error| format!("恢复 {} 文件模式失败：{error}", baseline.path))?;
        }
        Ok(())
    } else {
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    return Err(format!(
                        "{} 已变成目录，为避免误删已停止回滚",
                        baseline.path
                    ));
                }
                // `symlink_metadata` also sees dangling links, which
                // `Path::exists` misses.
                std::fs::remove_file(&target)
                    .map_err(|error| format!("删除 {} 失败：{error}", baseline.path))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("无法检查 {} 是否需要删除：{error}", baseline.path,));
            }
        }
        Ok(())
    }
}

/// Restore a single checkpoint file under the same native lease used by
/// closed-document writes. Stage bytes before a final hash/parent check so
/// both competing native edits and longer disk writes fail closed.
fn restore_one_if_current(
    root: &Path,
    baseline: &BaselineFile,
    expected: &str,
) -> Result<(), String> {
    let target = resolve_in_workspace(root, &baseline.path)?;
    ensure_safe_parent_chain(root, &target, false)?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let lease_path = resolve_in_workspace(&canonical_root, &baseline.path)?;
    let transaction = crate::coding_workspace::document_write_lock(&lease_path)?;
    let _guard = transaction
        .lock()
        .map_err(|_| "文档写入协调状态已损坏".to_string())?;
    if live_hash_for_path(root, &baseline.path)? != expected {
        return Err("恢复冲突：文件在预检之后已变化，已保留当前内容".into());
    }
    let metadata = std::fs::symlink_metadata(&target).ok();
    if metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() || metadata.permissions().readonly())
    {
        return Err("文件已变为目录或只读，已拒绝恢复".into());
    }
    if !baseline.existed {
        ensure_safe_parent_chain(root, &target, false)?;
        if live_hash_for_path(root, &baseline.path)? != expected {
            return Err("恢复冲突：删除前文件已变化，已保留当前内容".into());
        }
        if expected != "<deleted>" {
            std::fs::remove_file(&target).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let content = STANDARD
        .decode(
            baseline
                .content_base64
                .as_deref()
                .ok_or_else(|| "缺少可恢复快照".to_string())?,
        )
        .map_err(|error| error.to_string())?;
    ensure_safe_parent_chain(root, &target, true)?;
    let parent = target
        .parent()
        .ok_or_else(|| "文件没有父目录".to_string())?;
    let mut staged = tempfile::Builder::new()
        .prefix(".echo-restore-")
        .suffix(".coding.tmp")
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    staged
        .write_all(&content)
        .map_err(|error| error.to_string())?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    if let Some(metadata) = metadata.as_ref().filter(|metadata| metadata.is_file()) {
        staged
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| error.to_string())?;
    }
    #[cfg(unix)]
    if let Some(captured_mode) = baseline.mode.as_deref() {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = staged
            .as_file()
            .metadata()
            .map_err(|error| error.to_string())?
            .permissions();
        // A temporary file starts at 0600. When recreating a missing regular
        // file, use the captured Git mode's normal read/write permissions;
        // existing regular files retain their current read/write bits.
        let mode = if metadata.as_ref().is_some_and(|metadata| metadata.is_file()) {
            permissions.mode()
        } else {
            0o644
        };
        permissions.set_mode(if captured_mode == "100755" {
            mode | 0o111
        } else {
            mode & !0o111
        });
        staged
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| error.to_string())?;
    }
    let (file, staging) = staged.keep().map_err(|error| error.to_string())?;
    drop(file);
    let result = (|| {
        ensure_safe_parent_chain(root, &target, false)?;
        if live_hash_for_path(root, &baseline.path)? != expected {
            return Err("恢复冲突：准备替换期间文件已变化，已保留当前内容".into());
        }
        if std::fs::symlink_metadata(&target)
            .is_ok_and(|metadata| metadata.is_dir() || metadata.permissions().readonly())
        {
            return Err("文件已变为目录或只读，已拒绝替换".into());
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&target) {
            if metadata.is_file() {
                #[allow(unused_mut)]
                let mut permissions = metadata.permissions();
                #[cfg(unix)]
                if let Some(captured_mode) = baseline.mode.as_deref() {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = permissions.mode();
                    permissions.set_mode(if captured_mode == "100755" {
                        mode | 0o111
                    } else {
                        mode & !0o111
                    });
                }
                std::fs::set_permissions(&staging, permissions)
                    .map_err(|error| error.to_string())?;
            }
        }
        crate::paths::replace_file_atomically(&staging, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}

pub fn ensure_head_unchanged(root: &Path, set: &ChangeSet) -> Result<(), String> {
    if set.effective_baseline_mode() == BaselineMode::Filesystem {
        return Ok(());
    }
    let baseline = set
        .baseline_head
        .as_deref()
        .ok_or_else(|| "任务缺少安全基线".to_string())?;
    if git_head(root)? != baseline {
        return Err("任务期间 Git HEAD 已变更，自动回滚已停止，请手工检查差异".into());
    }
    Ok(())
}

fn baseline_for<'a>(set: &'a ChangeSet, path: &str) -> Result<&'a BaselineFile, String> {
    set.baseline_entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| format!("{path} 缺少任务前快照，已停止破坏性操作"))
}

/// Restore every task-related file to its exact task-start content. Returns the
/// paths that were restored or deleted.
#[cfg(test)]
pub fn rollback(root: &Path, task_id: &str) -> Result<Vec<String>, String> {
    // Legacy snapshot/topology tests exercise the storage primitive directly.
    // Every production bulk entry goes through receipt ownership below.
    rollback_inner(root, task_id, false)
}

fn rollback_agent_owned(root: &Path, task_id: &str) -> Result<Vec<String>, String> {
    rollback_inner(root, task_id, true)
}

fn ensure_rollback_receipts(root: &Path, set: &ChangeSet) -> Result<(), String> {
    let mut checkpoints =
        read_checkpoints(&store::task_dir(root, &set.task_id).join("checkpoints"));
    checkpoints.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    for change in &set.changes {
        let refuse = || {
            format!("{} 包含来源不明、交错或之后的用户修改，无法安全整体回滚；请使用本轮恢复或逐文件取舍", change.path)
        };
        let baseline = baseline_for(set, &change.path)?;
        let mut expected = baseline.hash.clone().unwrap_or_else(|| "<deleted>".into());
        let mut owned = false;
        for checkpoint in &checkpoints {
            if checkpoint.restored_at.is_some() {
                continue;
            }
            let Some(after) = checkpoint.after_hashes.get(&change.path) else {
                continue;
            };
            // Restores retire confirmed paths while retaining their historical
            // after hashes. A finished path in neither list was already
            // restored; it is not another live link in the ownership chain.
            if checkpoint.finished_at.is_some()
                && !checkpoint.confirmed_paths.contains(&change.path)
                && !checkpoint.external_paths.contains(&change.path)
                && !checkpoint.interleaved_paths.contains(&change.path)
            {
                continue;
            }
            if checkpoint.task_id != set.task_id
                || checkpoint.finished_at.is_none()
                || checkpoint.interleaved_paths.contains(&change.path)
                || checkpoint.external_paths.contains(&change.path)
                || !checkpoint.confirmed_paths.contains(&change.path)
            {
                return Err(refuse());
            }
            let before = checkpoint
                .before
                .iter()
                .find(|entry| entry.path == change.path)
                .and_then(|entry| entry.hash.as_deref())
                .unwrap_or("<deleted>");
            if before != expected {
                return Err(refuse());
            }
            expected = after.clone();
            owned = true;
        }
        if !owned || set.change_hashes.get(&change.path) != Some(&expected) {
            return Err(refuse());
        }
    }
    Ok(())
}

fn rollback_inner(
    root: &Path,
    task_id: &str,
    require_receipts: bool,
) -> Result<Vec<String>, String> {
    let mut set = load(root, task_id);
    ensure_head_unchanged(root, &set)?;
    if require_receipts {
        ensure_rollback_receipts(root, &set)?;
    }
    ensure_changes_current(root, &set)?;
    if !set.rollback_unsafe_files.is_empty() {
        return Err(format!(
            "以下文件没有安全快照，为避免数据丢失已取消整体回滚：{}",
            set.rollback_unsafe_files.join("、")
        ));
    }
    // Preflight every snapshot before touching the working tree, preventing a
    // half-applied rollback if persisted metadata is incomplete.
    for change in &set.changes {
        let baseline = baseline_for(&set, &change.path)?;
        if baseline.existed && baseline.content_base64.is_none() {
            return Err(format!("{} 缺少回滚内容", change.path));
        }
        if baseline.existed {
            let target = resolve_in_workspace(root, &baseline.path)?;
            if std::fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.is_dir()) {
                ensure_replacement_directory_is_task_owned(root, &target, &set)?;
            }
        }
    }
    let mut restored = Vec::new();
    // Delete task-created leaves first, then restore task-start files. This
    // handles both file→directory and directory→file replacements without
    // recursively deleting unknown content.
    for change in &set.changes {
        if baseline_for(&set, &change.path)?.existed {
            continue;
        }
        if require_receipts {
            let expected = set
                .change_hashes
                .get(&change.path)
                .ok_or_else(|| "恢复路径缺少内容版本".to_string())?;
            restore_one_if_current(root, baseline_for(&set, &change.path)?, expected)?;
        } else {
            restore_one(root, baseline_for(&set, &change.path)?)?;
        }
        restored.push(change.path.clone());
    }
    for change in &set.changes {
        if !baseline_for(&set, &change.path)?.existed {
            continue;
        }
        if require_receipts {
            let expected = set
                .change_hashes
                .get(&change.path)
                .ok_or_else(|| "恢复路径缺少内容版本".to_string())?;
            restore_one_if_current(root, baseline_for(&set, &change.path)?, expected)?;
        } else {
            restore_one(root, baseline_for(&set, &change.path)?)?;
        }
        restored.push(change.path.clone());
    }
    if require_receipts {
        for mut checkpoint in read_checkpoints(&store::task_dir(root, task_id).join("checkpoints"))
        {
            if checkpoint.task_id != task_id {
                continue;
            }
            let previous = checkpoint.confirmed_paths.len();
            checkpoint
                .confirmed_paths
                .retain(|path| !restored.contains(path));
            if previous == checkpoint.confirmed_paths.len() {
                continue;
            }
            if checkpoint.confirmed_paths.is_empty()
                && checkpoint.external_paths.is_empty()
                && checkpoint.interleaved_paths.is_empty()
            {
                checkpoint.restored_at = Some(chrono::Utc::now().to_rfc3339());
            }
            let path = checkpoint_path(root, task_id, &checkpoint.run_id)?;
            store::write_json(&path, &checkpoint)
                .map_err(|error| format!("文件已恢复，但更新回合恢复记录失败：{error}"))?;
        }
    }
    set.changes.clear();
    set.change_hashes.clear();
    set.verified_revision = None;
    set.verification_started_revision = None;
    set.reviewed_hashes.clear();
    set.reviewed_files.clear();
    save(root, &set)?;
    Ok(restored)
}

pub fn discard_file(root: &Path, task_id: &str, path: &str) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    ensure_head_unchanged(root, &set)?;
    let Some(index) = set.changes.iter().position(|change| change.path == path) else {
        return Ok(set);
    };
    let expected = set
        .change_hashes
        .get(path)
        .ok_or_else(|| "该文件缺少内容版本，已拒绝丢弃".to_string())?;
    if &live_hash_for_path(root, path)? != expected {
        return Err("文件在丢弃确认后又发生了变化，请刷新后重试".into());
    }
    if set.rollback_unsafe_files.iter().any(|entry| entry == path) {
        return Err("该文件没有安全快照，已拒绝丢弃".into());
    }
    restore_one_if_current(root, baseline_for(&set, path)?, expected)?;
    set.changes.remove(index);
    set.change_hashes.remove(path);
    set.verified_revision = None;
    set.verification_started_revision = None;
    set.reviewed_hashes.remove(path);
    set.reviewed_files.retain(|entry| entry != path);
    save(root, &set)?;
    Ok(set)
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDiff {
    pub original: String,
    pub modified: String,
    pub binary: bool,
    pub modified_hash: String,
    /// A full-document edit represents the entire discard only when both
    /// regular text files exist and no captured file mode needs restoring.
    pub can_apply_text: bool,
}

pub fn change_diff(root: &Path, task_id: &str, path: &str) -> Result<ChangeDiff, String> {
    let set = load(root, task_id);
    if !set.changes.iter().any(|change| change.path == path) {
        return Err("该文件不在当前任务变更集中".into());
    }
    let baseline = baseline_for(&set, path)?;
    let original_bytes = if baseline.existed {
        STANDARD
            .decode(
                baseline
                    .content_base64
                    .as_deref()
                    .ok_or_else(|| format!("{path} 缺少任务前快照"))?,
            )
            .map_err(|error| format!("解码 {path} 的任务前快照失败：{error}"))?
    } else {
        Vec::new()
    };
    let modified_bytes = match read_workspace_bytes(root, path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(ChangeDiff {
                original: "文件过大或不是普通文件：不显示文本差异".into(),
                modified: "可查看变更状态，但需使用专用工具审阅".into(),
                binary: true,
                can_apply_text: false,
                modified_hash: set
                    .change_hashes
                    .get(path)
                    .cloned()
                    .ok_or_else(|| "无法确认该差异的内容版本".to_string())?,
            });
        }
    };
    let text_metadata_unchanged = baseline.existed
        && modified_bytes.is_some()
        && baseline.mode.as_deref() == Some(executable_mode(&resolve_in_workspace(root, path)?));
    let modified_hash = modified_bytes.as_ref().map_or_else(
        || "<deleted>".to_string(),
        |bytes| live_content_hash(root, path, bytes),
    );
    let original = String::from_utf8(original_bytes);
    let modified = String::from_utf8(modified_bytes.unwrap_or_default());
    match (original, modified) {
        (Ok(original), Ok(modified)) if !original.contains('\0') && !modified.contains('\0') => {
            let can_apply_text = text_metadata_unchanged
                && original != modified
                && !original.starts_with('\u{feff}')
                && !modified.starts_with('\u{feff}')
                && original.len() <= 2_000_000
                && modified.len() <= 2_000_000;
            Ok(ChangeDiff {
                original,
                modified,
                binary: false,
                modified_hash,
                can_apply_text,
            })
        }
        _ => Ok(ChangeDiff {
            original: "二进制文件：不显示文本差异".into(),
            modified: "二进制文件：请使用专用工具审阅".into(),
            binary: true,
            modified_hash,
            can_apply_text: false,
        }),
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HunkRevertPreview {
    pub path: String,
    /// The complete current document. The editor must match this text and its
    /// own document version before applying the proposal.
    pub original: String,
    pub modified: String,
}

#[tauri::command]
pub async fn coding_changeset_revert_hunk_preview(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    path: String,
    hunk_id: String,
) -> Result<HunkRevertPreview, String> {
    let root = access.require_workspace(&root)?;
    sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || {
        crate::coding::delivery::revert_hunk_preview(&root, &task_id, &path, &hunk_id)
    })
    .await
    .map_err(|error| error.to_string())?
}

pub fn mark_committed_with_baselines(
    root: &Path,
    task_id: &str,
    hash: &str,
    paths: &[String],
    partial_baselines: &BTreeMap<String, String>,
) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    // Advance only the selected baselines. Unselected task changes and the
    // user's original dirty files retain their exact pre-commit snapshots.
    let mut snapshot_bytes = 0;
    for path in paths {
        let baseline = if let Some(content) = partial_baselines.get(path) {
            let output = crate::process_supervisor::background_sync_command("git")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .args(crate::coding_workspace::read_only_git_overrides(root)?)
                .args(["ls-tree", "-z", hash, "--", path])
                .current_dir(root)
                .output()
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!("无法读取部分提交的 {path} 元数据"));
            }
            if output.stdout.is_empty() {
                BaselineFile {
                    path: path.clone(),
                    mode: None,
                    existed: false,
                    content_base64: None,
                    hash: None,
                }
            } else {
                let header = String::from_utf8_lossy(&output.stdout);
                let captured_mode = set
                    .baseline_entries
                    .iter()
                    .find(|entry| &entry.path == path)
                    .and_then(|entry| entry.mode.clone())
                    .or_else(|| header.split_whitespace().next().map(str::to_string));
                BaselineFile {
                    mode: captured_mode.clone(),
                    path: path.clone(),
                    existed: true,
                    content_base64: Some(STANDARD.encode(content.as_bytes())),
                    hash: Some(content_hash_for_mode(
                        content.as_bytes(),
                        captured_mode.as_deref().unwrap_or("100644"),
                    )),
                }
            }
        } else {
            let output = crate::process_supervisor::background_sync_command("git")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .args(crate::coding_workspace::read_only_git_overrides(root)?)
                .args(["ls-tree", "-z", hash, "--", path])
                .current_dir(root)
                .output()
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!("无法读取已提交的 {path} 元数据"));
            }
            if output.stdout.is_empty() {
                BaselineFile {
                    mode: None,
                    path: path.clone(),
                    existed: false,
                    content_base64: None,
                    hash: None,
                }
            } else {
                let header = String::from_utf8_lossy(&output.stdout);
                let mode = header.split_whitespace().next().unwrap_or("100644");
                let content = crate::process_supervisor::background_sync_command("git")
                    .env("GIT_OPTIONAL_LOCKS", "0")
                    .args(crate::coding_workspace::read_only_git_overrides(root)?)
                    .args(["show", &format!("{hash}:{path}")])
                    .current_dir(root)
                    .output()
                    .map_err(|error| error.to_string())?;
                if !content.status.success() {
                    return Err(format!("无法读取已提交的 {path} 内容"));
                }
                snapshot_bytes += content.stdout.len();
                if snapshot_bytes > MAX_FILESYSTEM_SNAPSHOT_BYTES {
                    return Err("已提交快照超过安全大小限制".into());
                }
                BaselineFile {
                    mode: Some(mode.into()),
                    path: path.clone(),
                    existed: true,
                    content_base64: Some(STANDARD.encode(&content.stdout)),
                    hash: Some(content_hash_for_mode(&content.stdout, mode)),
                }
            }
        };
        set.baseline_entries.retain(|entry| &entry.path != path);
        set.baseline_entries.push(baseline);
        set.reviewed_hashes.remove(path);
        if !partial_baselines.contains_key(path) {
            set.changes.retain(|change| &change.path != path);
            set.change_hashes.remove(path);
        }
    }
    set.baseline_head = Some(hash.to_string());
    set.committed_hash = Some(hash.to_string());
    set.committed_paths = paths.to_vec();
    set.verified_revision = None;
    set.verification_started_revision = None;
    set.reviewed_files = set.reviewed_hashes.keys().cloned().collect();
    save(root, &set)?;
    Ok(set)
}

pub fn mark_verified_revision(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    let current = set.content_revision();
    if set.verification_started_revision.as_deref() != Some(current.as_str()) {
        return Err("文件在验证执行期间发生变化，请重新运行完整验证批次".into());
    }
    set.verified_revision = Some(current);
    set.verification_started_revision = None;
    save(root, &set)?;
    Ok(set)
}

pub fn mark_verification_started(root: &Path, task_id: &str) -> Result<ChangeSet, String> {
    let mut set = load(root, task_id);
    set.verification_started_revision = Some(set.content_revision());
    set.verified_revision = None;
    save(root, &set)?;
    Ok(set)
}

/// A turn checkpoint is distinct from the session's accumulated change set.
/// Only paths confirmed by actual Agent tool events are restorable; observation
/// of a filesystem delta alone never attributes that delta to the Agent.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct RunCheckpoint {
    run_id: String,
    #[serde(default)]
    mode: Option<String>,
    task_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    preserved_buffers: BTreeSet<String>,
    #[serde(default)]
    operation_receipts: BTreeSet<String>,
    #[serde(default)]
    interleaved_paths: BTreeSet<String>,
    baseline_head: Option<String>,
    created_at: String,
    finished_at: Option<String>,
    restored_at: Option<String>,
    before: Vec<BaselineFile>,
    after_hashes: BTreeMap<String, String>,
    confirmed_paths: Vec<String>,
    external_paths: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointView {
    pub run_id: String,
    pub task_id: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub restored_at: Option<String>,
    pub confirmed_paths: Vec<String>,
    pub external_paths: Vec<String>,
}

impl From<&RunCheckpoint> for CheckpointView {
    fn from(value: &RunCheckpoint) -> Self {
        Self {
            run_id: value.run_id.clone(),
            task_id: value.task_id.clone(),
            created_at: value.created_at.clone(),
            finished_at: value.finished_at.clone(),
            restored_at: value.restored_at.clone(),
            confirmed_paths: value.confirmed_paths.clone(),
            external_paths: value.external_paths.clone(),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRestoreResult {
    pub restored_paths: Vec<String>,
    pub conflicts: Vec<String>,
}

fn checkpoint_path(root: &Path, task_id: &str, run_id: &str) -> Result<PathBuf, String> {
    store::validate_task_id(task_id)?;
    store::validate_task_id(run_id)?;
    Ok(store::task_dir(root, task_id)
        .join("checkpoints")
        .join(format!("{run_id}.json")))
}

fn read_checkpoints(directory: &Path) -> Vec<RunCheckpoint> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension() == Some(std::ffi::OsStr::new("json")))
        .filter_map(|entry| store::read_json::<RunCheckpoint>(&entry.path()))
        .collect()
}

fn checkpoint_is_superseded(path: &Path, checkpoint: &RunCheckpoint) -> bool {
    path.parent().is_some_and(|directory| {
        read_checkpoints(directory).iter().any(|other| {
            other.run_id != checkpoint.run_id && other.created_at > checkpoint.created_at
        })
    })
}

fn untag_content_hash(hash: &str) -> &str {
    if hash == "missing" {
        "<deleted>"
    } else {
        hash.strip_prefix("100755:").unwrap_or(hash)
    }
}

pub fn checkpoint_start(
    root: &Path,
    task_id: &str,
    run_id: &str,
) -> Result<CheckpointView, String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    store::with_task_transaction(root, task_id, || {
        if let Some(existing) = store::read_json::<RunCheckpoint>(&path) {
            return Ok(CheckpointView::from(&existing));
        }
        if path.exists() {
            return Err("本轮检查点数据损坏，已保留原文件".into());
        }
        let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        let paths = workspace_paths(root)?;
        let mut bytes = 0;
        let before = paths
            .iter()
            .map(|path| checkpoint_baseline(root, path, &mut bytes))
            .collect::<Result<Vec<_>, _>>()?;
        ensure_checkpoint_still_current(root, &paths, &before)?;
        let checkpoint = RunCheckpoint {
            run_id: run_id.into(),
            mode: None,
            task_id: task_id.into(),
            session_id: task.session_id,
            preserved_buffers: BTreeSet::new(),
            operation_receipts: BTreeSet::new(),
            interleaved_paths: BTreeSet::new(),
            baseline_head: git_head(root).ok(),
            created_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            restored_at: None,
            before,
            after_hashes: BTreeMap::new(),
            confirmed_paths: Vec::new(),
            external_paths: Vec::new(),
        };
        store::write_json(&path, &checkpoint)?;
        Ok(CheckpointView::from(&checkpoint))
    })
}

/// Bound once by native prompt admission, never by a mutable composer selector.
pub(crate) fn checkpoint_bind_mode(
    root: &Path,
    task_id: &str,
    run_id: &str,
    mode: Option<&str>,
) -> Result<(), String> {
    checkpoint_start(root, task_id, run_id)?;
    let path = checkpoint_path(root, task_id, run_id)?;
    store::with_task_transaction(root, task_id, || {
        let mut checkpoint: RunCheckpoint = store::read_json(&path).ok_or("本轮检查点不存在")?;
        let mode = mode.unwrap_or("unknown");
        if checkpoint
            .mode
            .as_deref()
            .is_some_and(|existing| existing != mode)
        {
            return Err("本轮运行权限已绑定，不能修改".into());
        }
        checkpoint.mode = Some(mode.into());
        store::write_json(&path, &checkpoint)
    })
}

pub(crate) fn checkpoint_run_is_readonly(
    root: &Path,
    task_id: &str,
    run_id: &str,
) -> Result<bool, String> {
    let checkpoint: RunCheckpoint = store::read_json(&checkpoint_path(root, task_id, run_id)?)
        .ok_or("本轮检查点不存在，不能确认执行权限")?;
    Ok(checkpoint.mode.as_deref() != Some("agent"))
}

pub fn checkpoint_preserve_buffer(
    root: &Path,
    task_id: &str,
    run_id: &str,
    relative: &str,
    content: &str,
) -> Result<CheckpointView, String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    let target = resolve_in_workspace(root, relative)?;
    ensure_safe_parent_chain(root, &target, false)?;
    if content.len() > 2 * 1024 * 1024 || content.contains('\0') {
        return Err("仅支持保留 2 MiB 以内的文本 buffer；未保存内容仍留在编辑器中".into());
    }
    store::with_task_transaction(root, task_id, || {
        let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        let mut checkpoint = store::read_json::<RunCheckpoint>(&path)
            .ok_or_else(|| "本轮检查点不存在".to_string())?;
        if checkpoint.task_id != task_id
            || checkpoint.run_id != run_id
            || checkpoint.finished_at.is_some()
            || checkpoint.restored_at.is_some()
            || matches!(
                task.phase,
                TaskPhase::Stopped | TaskPhase::Paused | TaskPhase::Delivered | TaskPhase::Blocked
            )
        {
            return Err("本轮已结束或任务身份变化，已拒绝过期 buffer 快照".into());
        }
        if checkpoint
            .session_id
            .as_ref()
            .is_some_and(|session| task.session_id.as_ref() != Some(session))
        {
            return Err("检查点会话与当前任务会话不一致".into());
        }
        if checkpoint.preserved_buffers.contains(relative) {
            return Ok(CheckpointView::from(&checkpoint));
        }
        if checkpoint_is_superseded(&path, &checkpoint) {
            return Err("已有更新的运行占用此任务，已拒绝旧轮 buffer 快照".into());
        }
        let existing = checkpoint
            .before
            .iter()
            .find(|entry| entry.path == relative);
        if existing.is_some_and(|entry| entry.existed && entry.content_base64.is_none()) {
            return Err("该文件没有可安全覆盖的普通文本基线".into());
        }
        let mode = if existing
            .and_then(|entry| entry.hash.as_deref())
            .is_some_and(|hash| hash.starts_with("100755:"))
        {
            "100755"
        } else {
            "100644"
        };
        let baseline = BaselineFile {
            mode: existing
                .and_then(|entry| entry.mode.clone())
                .or_else(|| Some(mode.into())),
            path: relative.into(),
            existed: true,
            content_base64: Some(STANDARD.encode(content.as_bytes())),
            hash: Some(content_hash_for_mode(content.as_bytes(), mode)),
        };
        checkpoint.before.retain(|entry| entry.path != relative);
        checkpoint.before.push(baseline);
        checkpoint.preserved_buffers.insert(relative.into());
        if checkpoint.session_id.is_none() {
            checkpoint.session_id = task.session_id;
        }
        store::write_json(&path, &checkpoint)?;
        Ok(CheckpointView::from(&checkpoint))
    })
}

#[tauri::command]
pub async fn coding_checkpoint_preserve_buffer(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    run_id: String,
    path: String,
    content: String,
) -> Result<CheckpointView, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || {
        checkpoint_preserve_buffer(&root, &task_id, &run_id, &path, &content)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Native-only receipt for a successful, version-checked editor operation.
/// This survives a renderer/app interruption and never relies on a filename
/// merely appearing in model text or a filesystem watcher.
#[cfg(test)]
pub fn checkpoint_note_applied(
    root: &Path,
    task_id: &str,
    run_id: &str,
    relative: &str,
) -> Result<(), String> {
    checkpoint_note_operation_applied(
        root,
        task_id,
        run_id,
        relative,
        &uuid::Uuid::now_v7().to_string(),
        None,
        None,
    )
}

pub fn checkpoint_note_operation_applied(
    root: &Path,
    task_id: &str,
    run_id: &str,
    relative: &str,
    operation_id: &str,
    before_hash: Option<&str>,
    after_hash: Option<&str>,
) -> Result<(), String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    resolve_in_workspace(root, relative)?;
    if operation_id.is_empty() || operation_id.len() > 256 {
        return Err("修改凭据缺少有效 operationId".into());
    }
    store::with_task_transaction(root, task_id, || {
        let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        let mut checkpoint = store::read_json::<RunCheckpoint>(&path)
            .ok_or_else(|| "本轮检查点不存在".to_string())?;
        if checkpoint.task_id != task_id || checkpoint.run_id != run_id {
            return Err("检查点身份与修改凭据不一致".into());
        }
        if checkpoint
            .session_id
            .as_ref()
            .is_some_and(|session| task.session_id.as_ref() != Some(session))
        {
            return Err("修改凭据会话与任务不一致".into());
        }
        let receipt = format!("{operation_id}\0{relative}");
        if checkpoint.operation_receipts.contains(&receipt) {
            return Ok(());
        }
        if checkpoint.finished_at.is_some()
            || checkpoint.restored_at.is_some()
            || checkpoint_is_superseded(&path, &checkpoint)
        {
            return Err("检查点已结束或被替换，已拒绝过期修改凭据".into());
        }
        if checkpoint.operation_receipts.len() >= 8192 {
            return Err("本轮恢复凭据数量达到安全上限，请结束当前轮后继续".into());
        }
        let current = live_hash_for_path(root, relative)?;
        if after_hash.is_some_and(|after| untag_content_hash(after) != untag_content_hash(&current))
        {
            checkpoint.interleaved_paths.insert(relative.into());
        }
        let original = checkpoint
            .before
            .iter()
            .find(|entry| entry.path == relative);
        let prior = checkpoint
            .after_hashes
            .get(relative)
            .map(String::as_str)
            .or_else(|| original.and_then(|entry| entry.hash.as_deref()))
            .unwrap_or("<deleted>");
        if before_hash.is_some_and(|before| untag_content_hash(before) != untag_content_hash(prior))
        {
            checkpoint.interleaved_paths.insert(relative.into());
        }
        // Native document tools edit text and preserve executable metadata.
        // A mode change between native receipts therefore belongs to an
        // external actor and must survive a round restore too.
        let known_mode = checkpoint
            .after_hashes
            .get(relative)
            .map(|hash| hash.starts_with("100755:"))
            .or_else(|| {
                original
                    .and_then(|entry| entry.mode.as_deref())
                    .map(|mode| mode == "100755")
            });
        if known_mode.is_some_and(|executable| executable != current.starts_with("100755:"))
            && current != "<deleted>"
        {
            checkpoint.interleaved_paths.insert(relative.into());
        }
        checkpoint.after_hashes.insert(relative.into(), current);
        if !checkpoint
            .confirmed_paths
            .iter()
            .any(|path| path == relative)
        {
            checkpoint.confirmed_paths.push(relative.into());
        }
        checkpoint.operation_receipts.insert(receipt);
        if checkpoint.session_id.is_none() {
            checkpoint.session_id = task.session_id;
        }
        store::write_json(&path, &checkpoint)
    })
}

pub fn checkpoint_finish(
    root: &Path,
    task_id: &str,
    run_id: &str,
    _confirmed_paths: &[String],
) -> Result<CheckpointView, String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    store::with_task_transaction(root, task_id, || {
        let mut checkpoint = store::read_json::<RunCheckpoint>(&path)
            .ok_or_else(|| "本轮检查点不存在".to_string())?;
        if checkpoint.finished_at.is_some() {
            return Ok(CheckpointView::from(&checkpoint));
        }
        // A renderer filename is not evidence of a disk write. Only native
        // receipts can bind restoration to the exact content actually applied.
        let confirmed = checkpoint
            .confirmed_paths
            .iter()
            .map(|path| {
                resolve_in_workspace(root, path)?;
                Ok(path.clone())
            })
            .collect::<Result<BTreeSet<_>, String>>()?;
        let applied_hashes = checkpoint.after_hashes.clone();
        checkpoint.after_hashes.clear();
        checkpoint.confirmed_paths.clear();
        checkpoint.external_paths.clear();
        let before = checkpoint
            .before
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    entry.hash.clone().unwrap_or_else(|| "<deleted>".into()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let all = workspace_paths(root)?
            .into_iter()
            .chain(before.keys().cloned())
            .collect::<BTreeSet<_>>();
        for name in all {
            let live = live_hash_for_path(root, &name)?;
            if before.get(&name).map(String::as_str).unwrap_or("<deleted>") == live {
                continue;
            }
            checkpoint.after_hashes.insert(name.clone(), live);
            if confirmed.contains(&name)
                && !checkpoint.interleaved_paths.contains(&name)
                && applied_hashes
                    .get(&name)
                    .is_some_and(|hash| hash == checkpoint.after_hashes.get(&name).unwrap())
            {
                checkpoint.confirmed_paths.push(name.clone());
            } else {
                checkpoint.external_paths.push(name.clone());
            }
        }
        checkpoint.finished_at = Some(chrono::Utc::now().to_rfc3339());
        store::write_json(&path, &checkpoint)?;
        Ok(CheckpointView::from(&checkpoint))
    })
}

/// Observe an authenticated external tool's actual terminal settlement, even
/// after cancellation already finished the turn. This adds observations to
/// that exact checkpoint without refreshing Agent receipts or new-turn state.
pub fn checkpoint_observe_external_terminal(
    root: &Path,
    task_id: &str,
    run_id: &str,
) -> Result<(), String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    store::with_task_transaction(root, task_id, || {
        let mut checkpoint = store::read_json::<RunCheckpoint>(&path)
            .ok_or_else(|| "外部调用对应的回合检查点不存在".to_string())?;
        if checkpoint.task_id != task_id || checkpoint.run_id != run_id {
            return Err("外部调用与检查点身份不一致".into());
        }
        let before = checkpoint
            .before
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    entry.hash.as_deref().unwrap_or("<deleted>"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let paths = workspace_paths(root)?
            .into_iter()
            .chain(before.keys().cloned())
            .chain(checkpoint.after_hashes.keys().cloned())
            .collect::<BTreeSet<_>>();
        let mut external = checkpoint
            .external_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for name in paths {
            let live = live_hash_for_path(root, &name)?;
            let receipt = checkpoint.after_hashes.get(&name);
            if checkpoint.confirmed_paths.contains(&name)
                && receipt == Some(&live)
                && !checkpoint.interleaved_paths.contains(&name)
                && !external.contains(&name)
            {
                continue;
            }
            if before.get(&name).copied().unwrap_or("<deleted>") != live
                || receipt.is_some_and(|previous| previous != &live)
            {
                external.insert(name);
            }
        }
        checkpoint.external_paths = external.into_iter().collect();
        store::write_json(&path, &checkpoint)
    })
}

pub fn checkpoint_restore(
    root: &Path,
    task_id: &str,
    run_id: &str,
) -> Result<CheckpointRestoreResult, String> {
    let path = checkpoint_path(root, task_id, run_id)?;
    let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    if matches!(
        task.phase,
        TaskPhase::Discovering
            | TaskPhase::Implementing
            | TaskPhase::Verifying
            | TaskPhase::Repairing
    ) {
        return Err("请先停止当前运行，再恢复本轮修改".into());
    }
    if store::read_json::<RunCheckpoint>(&path)
        .is_some_and(|checkpoint| checkpoint.finished_at.is_none())
    {
        // Reconcile a genuinely interrupted turn from persisted native edit
        // receipts; commands themselves are never re-executed or replayed.
        checkpoint_finish(root, task_id, run_id, &[])?;
    }
    store::with_task_transaction(root, task_id, || {
        let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        if matches!(
            task.phase,
            TaskPhase::Discovering
                | TaskPhase::Implementing
                | TaskPhase::Verifying
                | TaskPhase::Repairing
        ) {
            return Err("请先停止当前运行，再恢复本轮修改".into());
        }
        let mut checkpoint = store::read_json::<RunCheckpoint>(&path)
            .ok_or_else(|| "本轮检查点不存在".to_string())?;
        if checkpoint.finished_at.is_none() {
            return Err("本轮仍未结束，不能恢复".into());
        }
        if checkpoint.restored_at.is_some() {
            return Ok(CheckpointRestoreResult {
                restored_paths: Vec::new(),
                conflicts: Vec::new(),
            });
        }
        if checkpoint.baseline_head.is_some() && git_head(root).ok() != checkpoint.baseline_head {
            return Err("本轮之后 Git HEAD 已变化；请使用 Git 历史恢复已提交内容".into());
        }
        let mut result = CheckpointRestoreResult {
            restored_paths: Vec::new(),
            conflicts: checkpoint.interleaved_paths.iter().cloned().collect(),
        };
        let mut baselines = Vec::new();
        for name in &checkpoint.confirmed_paths {
            if checkpoint.external_paths.contains(name)
                || checkpoint.interleaved_paths.contains(name)
            {
                result.conflicts.push(name.clone());
                continue;
            }
            let current = live_hash_for_path(root, name)?;
            let baseline = checkpoint
                .before
                .iter()
                .find(|entry| &entry.path == name)
                .cloned()
                .unwrap_or(BaselineFile {
                    mode: None,
                    path: name.clone(),
                    existed: false,
                    content_base64: None,
                    hash: None,
                });
            if current == baseline.hash.as_deref().unwrap_or("<deleted>") {
                // A repeated restore after a crash may find a file already
                // restored. Treat that exact state as success without writing.
                result.restored_paths.push(name.clone());
                continue;
            }
            if checkpoint.after_hashes.get(name) != Some(&current) {
                result.conflicts.push(name.clone());
                continue;
            }
            if baseline.existed && baseline.content_base64.is_none() {
                result.conflicts.push(name.clone());
                continue;
            }
            let target = resolve_in_workspace(root, name)?;
            ensure_safe_parent_chain(root, &target, false)?;
            if std::fs::symlink_metadata(&target).is_ok_and(|metadata| metadata.is_dir()) {
                result.conflicts.push(name.clone());
                continue;
            }
            baselines.push(baseline);
        }
        // Preflight all relevant paths before the first restore. A changed
        // user buffer/disk file is reported as a conflict and remains intact.
        for baseline in baselines {
            let expected = checkpoint
                .after_hashes
                .get(&baseline.path)
                .cloned()
                .unwrap_or_else(|| "<deleted>".into());
            match restore_one_if_current(root, &baseline, &expected) {
                Ok(()) => result.restored_paths.push(baseline.path),
                Err(_) => result.conflicts.push(baseline.path),
            }
        }
        checkpoint
            .confirmed_paths
            .retain(|name| !result.restored_paths.contains(name));
        if result.conflicts.is_empty() {
            checkpoint.restored_at = Some(chrono::Utc::now().to_rfc3339());
        }
        store::write_json(&path, &checkpoint)?;
        Ok(result)
    })
}

#[tauri::command]
pub async fn coding_checkpoint_start(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    run_id: String,
) -> Result<CheckpointView, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || checkpoint_start(&root, &task_id, &run_id))
        .await
        .map_err(|error| error.to_string())?
}
#[tauri::command]
pub async fn coding_checkpoint_finish(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    run_id: String,
    confirmed_paths: Option<Vec<String>>,
) -> Result<CheckpointView, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || {
        checkpoint_finish(
            &root,
            &task_id,
            &run_id,
            &confirmed_paths.unwrap_or_default(),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}
#[tauri::command]
pub async fn coding_checkpoint_list(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<CheckpointView>, String> {
    let root = access.require_workspace(&root)?;
    store::validate_task_id(&task_id)?;
    let directory = store::task_dir(&root, &task_id).join("checkpoints");
    let mut checkpoints = read_checkpoints(&directory)
        .iter()
        .map(CheckpointView::from)
        .collect::<Vec<_>>();
    checkpoints.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(checkpoints)
}
#[tauri::command]
pub async fn coding_checkpoint_restore(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    run_id: String,
) -> Result<CheckpointRestoreResult, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    let restore_root = root.clone();
    let restore_task = task_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        checkpoint_restore(&restore_root, &restore_task, &run_id)
    })
    .await
    .map_err(|error| error.to_string())??;
    sync_changes(&root, &task_id)
        .await
        .map_err(|error| format!("文件已恢复，但变更状态同步失败，请刷新后重试：{error}"))?;
    Ok(result)
}

#[tauri::command]
pub async fn coding_changeset_get(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || ChangeSetView::from(&load(&root, &task_id)))
        .await
        .map_err(|error| format!("读取变更集失败：{error}"))
}

#[tauri::command]
pub async fn coding_changeset_capture_baseline(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    dirty_files: Vec<String>,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    let snapshot = git_snapshot(&root).await;
    // Never trust a renderer-provided dirty-file list for a safety boundary.
    // It remains in the wire contract for backward compatibility only.
    let _ = dirty_files;
    let set = tokio::task::spawn_blocking(move || {
        if snapshot.has_git && snapshot.head.is_some() {
            // Re-read Git status inside the blocking capture so the checkpoint
            // and its dirty-file manifest come from the same consistency pass.
            capture_baseline(&root, &task_id, Vec::new())
        } else {
            capture_filesystem_baseline(&root, &task_id)
        }
    })
    .await
    .map_err(|error| format!("记录基线失败：{error}"))??;
    Ok(ChangeSetView::from(&set))
}

#[tauri::command]
pub async fn coding_changeset_diff(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    path: String,
) -> Result<ChangeDiff, String> {
    let root = access.require_workspace(&root)?;
    // Bind the displayed diff to a fresh native snapshot. This also invalidates
    // a previous review mark if an external editor changed the file.
    sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || change_diff(&root, &task_id, &path))
        .await
        .map_err(|error| format!("读取任务差异失败：{error}"))?
}

#[tauri::command]
pub async fn coding_changeset_mark_reviewed(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    path: String,
    expected_hash: String,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    // Refresh first so the explicit acknowledgement is bound to the exact
    // bytes currently displayed and cannot approve a stale diff.
    sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || {
        store::with_task_transaction(&root, &task_id, || {
            let set = mark_reviewed_if_current(&root, &task_id, &path, &expected_hash)?;
            Ok(ChangeSetView::from(&set))
        })
    })
    .await
    .map_err(|error| format!("标记差异已审阅失败：{error}"))?
}

#[tauri::command]
pub async fn coding_changeset_discard_file(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    path: String,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    sync_changes(&root, &task_id).await?;
    let set = tokio::task::spawn_blocking(move || {
        let coding_task = task::load(&root, &task_id).ok_or_else(|| "任务不存在".to_string())?;
        if !matches!(
            coding_task.phase,
            TaskPhase::Implementing | TaskPhase::Repairing
        ) {
            return Err("请先将任务重新进入实现阶段，再丢弃文件改动".into());
        }
        discard_file(&root, &task_id, &path)
    })
    .await
    .map_err(|error| format!("丢弃文件改动失败：{error}"))??;
    Ok(ChangeSetView::from(&set))
}

/// Record one file the Agent (or the user's own editor) just changed. The
/// frontend calls this as edits land so the change set stays authoritative
/// without polling Git.
#[tauri::command]
pub async fn coding_changeset_record_change(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    change: FileChange,
) -> Result<ChangeSetView, String> {
    let root = access.require_workspace(&root)?;
    let set = tokio::task::spawn_blocking(move || record_change(&root, &task_id, change))
        .await
        .map_err(|error| format!("记录文件变更失败：{error}"))??;
    Ok(ChangeSetView::from(&set))
}

#[tauri::command]
pub async fn coding_task_rollback(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<String>, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    // Re-scan immediately before the destructive operation so files changed
    // by an external editor cannot be omitted from the rollback decision.
    sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || {
        store::with_task_transaction(&root, &task_id, || {
            let current = task::load(&root, &task_id).ok_or_else(|| "任务不存在".to_string())?;
            if !matches!(
                current.phase,
                TaskPhase::Paused | TaskPhase::Stopped | TaskPhase::Delivered | TaskPhase::Blocked
            ) {
                return Err("任务正在执行或验证，请先停止后再回滚".into());
            }
            let restored = rollback_agent_owned(&root, &task_id)?;
            if let Some(mut coding_task) = task::load(&root, &task_id) {
                coding_task.phase = TaskPhase::Blocked;
                coding_task.phase_reason = Some("任务已回滚".into());
                coding_task.blocker = Some("任务文件已恢复到开始时的状态。".into());
                coding_task.next_action = None;
                for node in &mut coding_task.task_nodes {
                    node.status = TaskNodeStatus::Blocked;
                }
                task::save(&root, &coding_task)?;
            }
            Ok(restored)
        })
    })
    .await
    .map_err(|error| format!("回滚任务失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("coding-cs-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn init_git(root: &Path, files: &[(&str, &str)]) {
        std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(root)
            .output()
            .unwrap();
        for (path, content) in files {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        for args in [
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
            &["add", "."][..],
            &["commit", "-m", "init"][..],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(output.status.success(), "git command failed: {args:?}");
        }
    }

    #[test]
    fn review_rejects_a_version_changed_after_the_diff_was_displayed() {
        let root = temp_root();
        std::fs::write(root.join("source.txt"), "before\n").unwrap();
        capture_filesystem_baseline(&root, "task-review").unwrap();

        std::fs::write(root.join("source.txt"), "version one\n").unwrap();
        let first = sync_from_filesystem(&root, "task-review").unwrap();
        let displayed = change_diff(&root, "task-review", "source.txt").unwrap();
        assert_eq!(
            first.change_hashes.get("source.txt"),
            Some(&displayed.modified_hash)
        );

        std::fs::write(root.join("source.txt"), "version two\n").unwrap();
        let second = sync_from_filesystem(&root, "task-review").unwrap();
        assert!(mark_reviewed_if_current(
            &root,
            "task-review",
            "source.txt",
            &displayed.modified_hash,
        )
        .is_err());
        assert!(load(&root, "task-review").reviewed_files.is_empty());

        let current_hash = second.change_hashes.get("source.txt").unwrap();
        let reviewed =
            mark_reviewed_if_current(&root, "task-review", "source.txt", current_hash).unwrap();
        assert_eq!(reviewed.reviewed_files, vec!["source.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn git_readiness_distinguishes_non_repository_and_missing_initial_commit() {
        let plain = temp_root();
        let plain_error = git_head(&plain).unwrap_err();
        assert!(plain_error.contains("不是 Git 仓库"));

        let empty = temp_root();
        let initialized = std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&empty)
            .output()
            .unwrap();
        assert!(initialized.status.success());
        let empty_error = git_head(&empty).unwrap_err();
        assert!(empty_error.contains("还没有提交"));

        std::fs::remove_dir_all(&plain).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn legacy_missing_baseline_is_never_treated_as_a_filesystem_checkpoint() {
        let root = temp_root();
        std::fs::write(root.join("existing.txt"), "keep me").unwrap();

        let legacy = load(&root, "failed-before-baseline");
        assert!(legacy.baseline_mode.is_none());
        assert!(legacy.baseline_head.is_none());
        assert_eq!(legacy.effective_baseline_mode(), BaselineMode::Git);
        assert!(sync_from_filesystem(&root, "failed-before-baseline").is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("existing.txt")).unwrap(),
            "keep me"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn plain_folder_checkpoint_tracks_and_rolls_back_agent_changes() {
        let root = temp_root();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/existing.html"),
            "<!-- old -->\n<body>ok</body>\n",
        )
        .unwrap();

        let baseline = capture_filesystem_baseline(&root, "task-local").unwrap();
        assert_eq!(baseline.effective_baseline_mode(), BaselineMode::Filesystem);
        assert!(baseline.baseline_head.is_none());

        std::fs::write(root.join("src/existing.html"), "<body>ok</body>\n").unwrap();
        std::fs::write(root.join("src/created.css"), "body { color: red; }\n").unwrap();

        let set = sync_from_filesystem(&root, "task-local").unwrap();
        assert_eq!(set.changes.len(), 2);
        assert!(set.changes.iter().any(
            |change| change.path == "src/existing.html" && change.kind == ChangeKind::Modified
        ));
        assert!(set
            .changes
            .iter()
            .any(|change| change.path == "src/created.css" && change.kind == ChangeKind::Added));
        assert!(set.rollback_unsafe_files.is_empty());

        let restored = rollback(&root, "task-local").unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(
            std::fs::read_to_string(root.join("src/existing.html")).unwrap(),
            "<!-- old -->\n<body>ok</body>\n"
        );
        assert!(!root.join("src/created.css").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn plain_folder_checkpoint_protects_files_ignored_at_task_start() {
        let root = temp_root();
        std::fs::write(root.join(".gitignore"), "private.txt\n").unwrap();
        std::fs::write(root.join("private.txt"), "user content\n").unwrap();

        let baseline = capture_filesystem_baseline(&root, "task-ignored").unwrap();
        assert!(baseline
            .baseline_entries
            .iter()
            .any(|entry| entry.path == "private.txt" && entry.existed));

        // Changing the ignore rule must not make this old file look newly
        // created, otherwise rollback would delete user data.
        std::fs::write(root.join(".gitignore"), "").unwrap();
        std::fs::write(root.join("private.txt"), "agent content\n").unwrap();
        let set = sync_from_filesystem(&root, "task-ignored").unwrap();
        assert!(set
            .changes
            .iter()
            .any(|change| { change.path == "private.txt" && change.kind == ChangeKind::Modified }));

        rollback(&root, "task-ignored").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("private.txt")).unwrap(),
            "user content\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_existing_checkpoint_is_idempotent_and_cannot_be_rebased_later() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "task start\n").unwrap();
        capture_filesystem_baseline(&root, "task-idempotent").unwrap();

        std::fs::write(root.join("a.txt"), "later edit\n").unwrap();
        capture_filesystem_baseline(&root, "task-idempotent").unwrap();
        sync_from_filesystem(&root, "task-idempotent").unwrap();
        rollback(&root, "task-idempotent").unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "task start\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn checkpoint_consistency_check_detects_a_concurrent_file_edit() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "first\n").unwrap();
        let initial_paths = workspace_paths(&root).unwrap();
        let mut snapshot_bytes = 0;
        let entries = initial_paths
            .iter()
            .map(|path| checkpoint_baseline(&root, path, &mut snapshot_bytes).unwrap())
            .collect::<Vec<_>>();

        std::fs::write(root.join("a.txt"), "second\n").unwrap();
        let error = ensure_checkpoint_still_current(&root, &initial_paths, &entries).unwrap_err();
        assert!(error.contains("建立检查点期间"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn git_checkpoint_protects_preexisting_ignored_files() {
        let root = temp_root();
        init_git(
            &root,
            &[(".gitignore", "private.txt\n"), ("README.md", "seed\n")],
        );
        std::fs::write(root.join("private.txt"), "user content\n").unwrap();

        let baseline = capture_baseline(&root, "task-git-ignored", Vec::new()).unwrap();
        assert!(baseline.is_pre_existing("private.txt"));

        std::fs::write(root.join(".gitignore"), "").unwrap();
        std::fs::write(root.join("private.txt"), "agent content\n").unwrap();
        let set = sync_from_git(&root, "task-git-ignored").await.unwrap();
        let private = set
            .changes
            .iter()
            .find(|change| change.path == "private.txt")
            .unwrap();
        assert_eq!(private.kind, ChangeKind::Modified);
        assert!(private.pre_existing);

        rollback(&root, "task-git-ignored").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("private.txt")).unwrap(),
            "user content\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn git_rollback_restores_a_file_replaced_by_a_directory_tree() {
        let root = temp_root();
        init_git(&root, &[("entry", "original file\n")]);
        capture_baseline(&root, "task-git-topology", Vec::new()).unwrap();

        std::fs::remove_file(root.join("entry")).unwrap();
        std::fs::create_dir(root.join("entry")).unwrap();
        std::fs::write(root.join("entry/created.txt"), "new\n").unwrap();
        let set = sync_from_git(&root, "task-git-topology").await.unwrap();
        assert!(set.rollback_unsafe_files.is_empty());

        rollback(&root, "task-git-topology").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("entry")).unwrap(),
            "original file\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn compact_view_never_serializes_rollback_contents() {
        let mut set = ChangeSet {
            task_id: "task-view".into(),
            baseline_mode: Some(BaselineMode::Filesystem),
            ..ChangeSet::default()
        };
        set.baseline_entries.push(BaselineFile {
            mode: None,
            path: "secret.txt".into(),
            existed: true,
            content_base64: Some(STANDARD.encode("do not cross IPC")),
            hash: Some("baseline-hash".into()),
        });
        set.changes.push(FileChange {
            path: "secret.txt".into(),
            kind: ChangeKind::Modified,
            added: 1,
            removed: 1,
            baseline_content: Some("also private".into()),
            pre_existing: true,
        });
        set.change_hashes
            .insert("secret.txt".into(), "current-hash".into());

        let json = serde_json::to_string(&ChangeSetView::from(&set)).unwrap();
        assert!(!json.contains("baselineEntries"));
        assert!(!json.contains("contentBase64"));
        assert!(!json.contains("baselineContent"));
        assert!(!json.contains("changeHashes"));
        assert!(json.contains("secret.txt"));
    }

    #[test]
    fn review_mark_is_rejected_when_live_content_changed_after_sync() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "before\n").unwrap();
        capture_filesystem_baseline(&root, "task-review").unwrap();
        std::fs::write(root.join("a.txt"), "first change\n").unwrap();
        sync_from_filesystem(&root, "task-review").unwrap();
        std::fs::write(root.join("a.txt"), "second change\n").unwrap();

        let error = mark_reviewed(&root, "task-review", "a.txt").unwrap_err();
        assert!(error.contains("重新打开并审阅"));
        assert!(!load(&root, "task-review").is_reviewed("a.txt"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn verification_revision_is_invalidated_by_later_file_changes() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "before\n").unwrap();
        capture_filesystem_baseline(&root, "task-verify").unwrap();
        std::fs::write(root.join("a.txt"), "first change\n").unwrap();
        sync_from_filesystem(&root, "task-verify").unwrap();

        mark_verification_started(&root, "task-verify").unwrap();
        let verified = mark_verified_revision(&root, "task-verify").unwrap();
        assert!(verified.verification_is_current());

        mark_verification_started(&root, "task-verify").unwrap();
        std::fs::write(root.join("a.txt"), "changed during verification\n").unwrap();
        let changed = sync_from_filesystem(&root, "task-verify").unwrap();
        assert!(!changed.verification_is_current());
        let error = mark_verified_revision(&root, "task-verify").unwrap_err();
        assert!(error.contains("验证执行期间"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rollback_restores_a_file_replaced_by_a_directory_tree() {
        let root = temp_root();
        std::fs::write(root.join("entry"), "original file\n").unwrap();
        capture_filesystem_baseline(&root, "task-topology").unwrap();

        std::fs::remove_file(root.join("entry")).unwrap();
        std::fs::create_dir(root.join("entry")).unwrap();
        std::fs::create_dir(root.join("entry/empty")).unwrap();
        std::fs::write(root.join("entry/created.txt"), "new\n").unwrap();
        sync_from_filesystem(&root, "task-topology").unwrap();

        rollback(&root, "task-topology").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("entry")).unwrap(),
            "original file\n"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn records_changes_and_accumulates_line_counts() {
        let root = temp_root();
        record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 10,
                removed: 2,
                baseline_content: Some("old".into()),
                pre_existing: false,
            },
        )
        .unwrap();
        let set = record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/b.ts".into(),
                kind: ChangeKind::Added,
                added: 30,
                removed: 0,
                baseline_content: None,
                pre_existing: false,
            },
        )
        .unwrap();
        assert_eq!(set.changes.len(), 2);
        assert_eq!(set.total_added(), 40);
        assert_eq!(set.total_removed(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn re_recording_same_file_replaces_not_duplicates() {
        let root = temp_root();
        record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 5,
                removed: 1,
                baseline_content: Some("v1".into()),
                pre_existing: false,
            },
        )
        .unwrap();
        let set = record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 8,
                removed: 3,
                baseline_content: Some("should-not-overwrite".into()),
                pre_existing: false,
            },
        )
        .unwrap();
        assert_eq!(set.changes.len(), 1);
        assert_eq!(set.changes[0].added, 8);
        // The first baseline must win; it is the only content that can restore
        // the file to its pre-task state.
        assert_eq!(set.changes[0].baseline_content.as_deref(), Some("v1"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn workspace_path_validation_rejects_traversal_but_allows_dotted_names() {
        let root = temp_root();
        assert!(resolve_in_workspace(&root, "../outside").is_err());
        assert!(resolve_in_workspace(&root, "/outside").is_err());
        assert_eq!(
            resolve_in_workspace(&root, "src/range..test.ts").unwrap(),
            root.join("src/range..test.ts")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn restore_never_follows_a_symlink_outside_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        let outside = temp_root();
        std::fs::write(outside.join("victim.txt"), "outside").unwrap();
        symlink(&outside, root.join("linked")).unwrap();
        let baseline = BaselineFile {
            mode: None,
            path: "linked/victim.txt".into(),
            existed: true,
            content_base64: Some(STANDARD.encode("baseline")),
            hash: Some(hash_bytes(b"baseline")),
        };

        assert!(restore_one(&root, &baseline).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("victim.txt")).unwrap(),
            "outside"
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rollback_removes_a_dangling_link_created_by_the_task() {
        use std::os::unix::fs::symlink;

        let root = temp_root();
        symlink(root.join("missing-target"), root.join("created-link")).unwrap();
        let baseline = BaselineFile {
            mode: None,
            path: "created-link".into(),
            existed: false,
            content_base64: None,
            hash: None,
        };

        restore_one(&root, &baseline).unwrap();
        assert!(std::fs::symlink_metadata(root.join("created-link")).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rollback_restores_exact_task_start_state_including_dirty_files() {
        let root = temp_root();
        init_git(
            &root,
            &[
                ("src/modified.ts", "original version"),
                ("src/user.ts", "head"),
            ],
        );
        std::fs::write(root.join("src/user.ts"), "user edited").unwrap();
        capture_baseline(&root, "task-1", vec!["src/user.ts".into()]).unwrap();

        std::fs::write(root.join("src/modified.ts"), "agent version").unwrap();
        std::fs::write(root.join("src/created.ts"), "agent created").unwrap();
        std::fs::write(root.join("src/user.ts"), "agent replaced user work").unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(sync_from_git(&root, "task-1")).unwrap();

        let restored = rollback(&root, "task-1").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/modified.ts")).unwrap(),
            "original version"
        );
        assert!(!root.join("src/created.ts").exists());
        // The user's exact task-start content, rather than HEAD, is restored.
        assert_eq!(
            std::fs::read_to_string(root.join("src/user.ts")).unwrap(),
            "user edited"
        );
        assert_eq!(restored.len(), 3);
        assert!(load(&root, "task-1").changes.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn baseline_marks_pre_existing_dirty_files() {
        let root = temp_root();
        init_git(&root, &[("README.md", "seed")]);
        let set = capture_baseline(
            &root,
            "task-1",
            vec!["src/dirty.ts".to_string(), "src/other.ts".to_string()],
        )
        .unwrap();
        assert_eq!(set.baseline_files.len(), 2);
        assert!(set.is_pre_existing("src/dirty.ts"));
        assert!(!set.is_pre_existing("src/fresh.ts"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mark_reviewed_is_idempotent() {
        let root = temp_root();
        record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Added,
                added: 1,
                removed: 0,
                baseline_content: None,
                pre_existing: false,
            },
        )
        .unwrap();
        mark_reviewed(&root, "task-1", "src/a.ts").unwrap();
        let set = mark_reviewed(&root, "task-1", "src/a.ts").unwrap();
        assert_eq!(set.reviewed_files, vec!["src/a.ts".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discard_file_restores_one_file_and_drops_its_record() {
        let root = temp_root();
        init_git(&root, &[("src/a.ts", "original")]);
        capture_baseline(&root, "task-1", Vec::new()).unwrap();
        std::fs::write(root.join("src/a.ts"), "agent").unwrap();
        record_change(
            &root,
            "task-1",
            FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 1,
                removed: 1,
                baseline_content: Some("original".into()),
                pre_existing: false,
            },
        )
        .unwrap();
        let set = discard_file(&root, "task-1", "src/a.ts").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.ts")).unwrap(),
            "original"
        );
        assert!(set.changes.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn sync_from_git_captures_untracked_files_as_added() {
        let root = temp_root();
        init_git(&root, &[("src/seed.ts", "seed\n")]);

        // User's pre-existing dirty edit — must stay protected.
        std::fs::write(root.join("src/seed.ts"), "seed\nuser-edit\n").unwrap();
        capture_baseline(&root, "task-1", vec!["src/seed.ts".to_string()])
            .map_err(|e| {
                eprintln!("baseline error: {e}");
                e
            })
            .unwrap();

        // The Agent creates a new file and edits an existing tracked file.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new.ts"), "export const x = 1;\n").unwrap();
        std::fs::write(root.join("src/seed.ts"), "seed\nagent-edit\n").unwrap();

        let set = sync_from_git(&root, "task-1").await.unwrap();
        let paths: Vec<&str> = set.changes.iter().map(|c| c.path.as_str()).collect();
        // The untracked new file must appear as a task change.
        assert!(
            paths.contains(&"src/new.ts"),
            "new file must appear: {paths:?}"
        );
        // Changes made on top of a dirty file must still be reviewable and
        // rollback restores its exact task-start bytes.
        assert!(
            paths.contains(&"src/seed.ts"),
            "changed dirty file must appear in task changes: {paths:?}"
        );
        assert!(
            set.changes
                .iter()
                .find(|change| change.path == "src/seed.ts")
                .unwrap()
                .pre_existing
        );
        assert_eq!(
            set.baseline_files,
            vec!["src/seed.ts".to_string()],
            "the user's dirty file is the baseline"
        );

        // Intent-to-add must be reversed: the new file is not staged.
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&root)
            .output()
            .unwrap();
        let staged_stdout = String::from_utf8_lossy(&staged.stdout).to_string();
        assert!(
            !staged_stdout.contains("src/new.ts"),
            "untracked file must not be staged after sync, got:\n{staged_stdout}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn sync_expands_new_directories_into_reviewable_files() {
        let root = temp_root();
        init_git(&root, &[("README.md", "seed")]);
        capture_baseline(&root, "task-1", Vec::new()).unwrap();
        std::fs::create_dir_all(root.join("new/nested")).unwrap();
        std::fs::write(root.join("new/nested/a.ts"), "one\ntwo\n").unwrap();

        let set = sync_from_git(&root, "task-1").await.unwrap();
        assert_eq!(set.changes.len(), 1);
        assert_eq!(set.changes[0].path, "new/nested/a.ts");
        assert_eq!(set.changes[0].kind, ChangeKind::Added);
        assert!(set.rollback_unsafe_files.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn line_totals_are_relative_to_the_exact_task_baseline() {
        let root = temp_root();
        init_git(&root, &[("src/a.ts", "one\ntwo\nthree\n")]);
        capture_baseline(&root, "task-1", Vec::new()).unwrap();
        std::fs::write(root.join("src/a.ts"), "one\nTWO\nthree\nfour\n").unwrap();

        let set = sync_from_git(&root, "task-1").await.unwrap();
        assert_eq!(set.changes.len(), 1);
        assert_eq!(set.changes[0].added, 2);
        assert_eq!(set.changes[0].removed, 1);
        std::fs::remove_dir_all(&root).ok();
    }
    #[cfg(unix)]
    #[test]
    fn executable_bit_changes_expire_evidence_and_restore_the_original_mode() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("script.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        capture_filesystem_baseline(root, "mode-task").unwrap();
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let changed = sync_from_filesystem(root, "mode-task").unwrap();
        assert_eq!(changed.changes.len(), 1);
        assert_eq!(changed.changes[0].added, 0);
        assert!(changed.change_hashes["script.sh"].starts_with("100755:"));
        mark_verification_started(root, "mode-task").unwrap();
        mark_verified_revision(root, "mode-task").unwrap();
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!sync_from_filesystem(root, "mode-task")
            .unwrap()
            .verification_is_current());
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        sync_from_filesystem(root, "mode-task").unwrap();
        rollback(root, "mode-task").unwrap();
        assert_eq!(
            std::fs::metadata(root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }

    #[test]
    fn turn_restore_preserves_user_changes_and_reports_later_overlaps() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("agent.txt"), "initial user content\n").unwrap();
        std::fs::write(root.join("external.txt"), "initial external\n").unwrap();
        let task = task::create_task(root, "Checkpoints", "Edit agent file").unwrap();
        checkpoint_start(root, &task.id, "run-one").unwrap();
        std::fs::write(root.join("agent.txt"), "agent round one\n").unwrap();
        std::fs::write(root.join("external.txt"), "user edited independently\n").unwrap();
        checkpoint_note_applied(root, &task.id, "run-one", "agent.txt").unwrap();
        let finished = checkpoint_finish(root, &task.id, "run-one", &["agent.txt".into()]).unwrap();
        assert_eq!(finished.confirmed_paths, ["agent.txt"]);
        assert_eq!(finished.external_paths, ["external.txt"]);
        std::fs::write(root.join("agent.txt"), "user typed after the Agent\n").unwrap();
        let conflict = checkpoint_restore(root, &task.id, "run-one").unwrap();
        assert_eq!(conflict.conflicts, ["agent.txt"]);
        assert_eq!(
            std::fs::read_to_string(root.join("agent.txt")).unwrap(),
            "user typed after the Agent\n"
        );
        std::fs::write(root.join("agent.txt"), "agent round one\n").unwrap();
        let restored = checkpoint_restore(root, &task.id, "run-one").unwrap();
        assert_eq!(restored.restored_paths, ["agent.txt"]);
        assert_eq!(
            std::fs::read_to_string(root.join("agent.txt")).unwrap(),
            "initial user content\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("external.txt")).unwrap(),
            "user edited independently\n"
        );
        assert!(checkpoint_restore(root, &task.id, "run-one")
            .unwrap()
            .restored_paths
            .is_empty());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn git_checkpoint_detects_mode_only_changes_and_restores_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        init_git(root, &[("script.sh", "#!/bin/sh\nexit 0\n")]);
        capture_baseline(root, "git-mode-task", Vec::new()).unwrap();
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let set = sync_from_git(root, "git-mode-task").await.unwrap();
        assert_eq!(set.changes.len(), 1);
        assert!(set.change_hashes["script.sh"].starts_with("100755:"));
        rollback(root, "git-mode-task").unwrap();
        assert_eq!(
            std::fs::metadata(root.join("script.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
    #[test]
    fn turn_restore_preserves_the_original_dirty_editor_buffer_once() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "old disk text\n").unwrap();
        let mut coding_task = task::create_task(root, "Dirty buffer", "Edit selection").unwrap();
        coding_task.phase = TaskPhase::Implementing;
        coding_task.session_id = Some("buffer-session".into());
        task::save(root, &coding_task).unwrap();
        checkpoint_start(root, &coding_task.id, "buffer-run").unwrap();
        checkpoint_preserve_buffer(
            root,
            &coding_task.id,
            "buffer-run",
            "source.ts",
            "user unsaved content\n",
        )
        .unwrap();
        checkpoint_preserve_buffer(
            root,
            &coding_task.id,
            "buffer-run",
            "source.ts",
            "duplicate event must not replace the original\n",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "old disk text\n",
            "preservation does not implicitly save an editor buffer"
        );
        std::fs::write(root.join("source.ts"), "Agent edit based on user buffer\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "buffer-run", "source.ts").unwrap();
        checkpoint_finish(root, &coding_task.id, "buffer-run", &["source.ts".into()]).unwrap();
        assert!(checkpoint_preserve_buffer(
            root,
            &coding_task.id,
            "buffer-run",
            "source.ts",
            "late buffer"
        )
        .is_err());
        coding_task.phase = TaskPhase::Delivered;
        task::save(root, &coding_task).unwrap();
        let result = checkpoint_restore(root, &coding_task.id, "buffer-run").unwrap();
        assert_eq!(result.restored_paths, ["source.ts"]);
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user unsaved content\n"
        );
    }
    #[test]
    fn interrupted_turn_recovers_from_native_receipts_without_replaying_writes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "user starting content\n").unwrap();
        let mut coding_task = task::create_task(root, "Crash recovery", "edit source").unwrap();
        coding_task.phase = TaskPhase::Implementing;
        task::save(root, &coding_task).unwrap();
        checkpoint_start(root, &coding_task.id, "interrupted-run").unwrap();
        std::fs::write(root.join("source.ts"), "actual completed edit\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "interrupted-run", "source.ts").unwrap();
        coding_task.phase = TaskPhase::Stopped;
        task::save(root, &coding_task).unwrap();
        // No renderer finish event arrived. A settled task reconciles the
        // persisted receipt and restores only the already-confirmed write.
        let result = checkpoint_restore(root, &coding_task.id, "interrupted-run").unwrap();
        assert_eq!(result.restored_paths, ["source.ts"]);
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user starting content\n"
        );
    }

    #[test]
    fn user_write_after_an_agent_receipt_is_external_and_not_rolled_back() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "starting content\n").unwrap();
        let coding_task = task::create_task(root, "Attribution", "edit source").unwrap();
        checkpoint_start(root, &coding_task.id, "mixed-run").unwrap();
        std::fs::write(root.join("source.ts"), "Agent edit\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "mixed-run", "source.ts").unwrap();
        std::fs::write(root.join("source.ts"), "user typed after the edit\n").unwrap();
        let finished =
            checkpoint_finish(root, &coding_task.id, "mixed-run", &["source.ts".into()]).unwrap();
        assert!(finished.confirmed_paths.is_empty());
        assert_eq!(finished.external_paths, ["source.ts"]);
        assert!(checkpoint_restore(root, &coding_task.id, "mixed-run")
            .unwrap()
            .restored_paths
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user typed after the edit\n"
        );
    }
    #[test]
    fn interleaved_user_content_between_agent_operations_survives_restore() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let initial = "starting content\n";
        std::fs::write(root.join("source.ts"), initial).unwrap();
        let coding_task = task::create_task(root, "Interleaving", "edit source").unwrap();
        checkpoint_start(root, &coding_task.id, "interleaved-run").unwrap();
        std::fs::write(root.join("source.ts"), "Agent first edit\n").unwrap();
        checkpoint_note_operation_applied(
            root,
            &coding_task.id,
            "interleaved-run",
            "source.ts",
            "edit-one",
            Some(&hash_bytes(initial.as_bytes())),
            None,
        )
        .unwrap();
        let user_content = "Agent first edit plus user typing\n";
        std::fs::write(root.join("source.ts"), user_content).unwrap();
        let final_content = "Agent second edit preserving user typing\n";
        std::fs::write(root.join("source.ts"), final_content).unwrap();
        checkpoint_note_operation_applied(
            root,
            &coding_task.id,
            "interleaved-run",
            "source.ts",
            "edit-two",
            Some(&hash_bytes(user_content.as_bytes())),
            None,
        )
        .unwrap();
        let finished = checkpoint_finish(
            root,
            &coding_task.id,
            "interleaved-run",
            &["source.ts".into()],
        )
        .unwrap();
        assert!(finished.confirmed_paths.is_empty());
        assert_eq!(finished.external_paths, ["source.ts"]);
        let restored = checkpoint_restore(root, &coding_task.id, "interleaved-run").unwrap();
        assert_eq!(restored.conflicts, ["source.ts"]);
        assert!(restored.restored_paths.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            final_content
        );
    }

    #[test]
    fn replayed_operation_receipt_does_not_claim_a_later_user_edit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let initial = "starting content\n";
        std::fs::write(root.join("source.ts"), initial).unwrap();
        let coding_task = task::create_task(root, "Receipts", "edit source").unwrap();
        checkpoint_start(root, &coding_task.id, "receipt-run").unwrap();
        std::fs::write(root.join("source.ts"), "Agent edit\n").unwrap();
        checkpoint_note_operation_applied(
            root,
            &coding_task.id,
            "receipt-run",
            "source.ts",
            "same-operation",
            Some(&hash_bytes(initial.as_bytes())),
            None,
        )
        .unwrap();
        let checkpoint_path = checkpoint_path(root, &coding_task.id, "receipt-run").unwrap();
        let original_receipt = store::read_json::<RunCheckpoint>(&checkpoint_path).unwrap();
        std::fs::write(root.join("source.ts"), "later user edit\n").unwrap();
        checkpoint_note_operation_applied(
            root,
            &coding_task.id,
            "receipt-run",
            "source.ts",
            "same-operation",
            Some(&hash_bytes(initial.as_bytes())),
            None,
        )
        .unwrap();
        let repeated = store::read_json::<RunCheckpoint>(&checkpoint_path).unwrap();
        assert_eq!(repeated.operation_receipts.len(), 1);
        assert_eq!(repeated.after_hashes, original_receipt.after_hashes);
        let finished =
            checkpoint_finish(root, &coding_task.id, "receipt-run", &["source.ts".into()]).unwrap();
        assert!(finished.confirmed_paths.is_empty());
        assert_eq!(finished.external_paths, ["source.ts"]);
        checkpoint_restore(root, &coding_task.id, "receipt-run").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "later user edit\n"
        );
    }

    #[test]
    fn checkpoint_inventory_only_reads_json_and_never_locks_lockfiles() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "initial\n").unwrap();
        let coding_task = task::create_task(root, "Inventory", "read checkpoints").unwrap();
        checkpoint_start(root, &coding_task.id, "inventory-run").unwrap();
        let path = checkpoint_path(root, &coding_task.id, "inventory-run").unwrap();
        let checkpoint_directory = path.parent().unwrap();
        for _ in 0..3 {
            let checkpoints = read_checkpoints(checkpoint_directory);
            assert_eq!(checkpoints.len(), 1);
            assert!(!checkpoint_is_superseded(&path, &checkpoints[0]));
        }
        assert!(std::fs::read_dir(checkpoint_directory)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".lock.lock")));
    }
    #[test]
    fn user_write_between_editor_apply_and_native_receipt_is_external() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let initial = "initial\n";
        std::fs::write(root.join("source.ts"), initial).unwrap();
        let coding_task = task::create_task(root, "After receipt", "edit source").unwrap();
        checkpoint_start(root, &coding_task.id, "after-run").unwrap();
        let agent_applied = "Agent applied\n";
        std::fs::write(
            root.join("source.ts"),
            "user write before native response\n",
        )
        .unwrap();
        checkpoint_note_operation_applied(
            root,
            &coding_task.id,
            "after-run",
            "source.ts",
            "after-operation",
            Some(&hash_bytes(initial.as_bytes())),
            Some(&hash_bytes(agent_applied.as_bytes())),
        )
        .unwrap();
        let finished =
            checkpoint_finish(root, &coding_task.id, "after-run", &["source.ts".into()]).unwrap();
        assert!(finished.confirmed_paths.is_empty());
        assert_eq!(finished.external_paths, ["source.ts"]);
        let restored = checkpoint_restore(root, &coding_task.id, "after-run").unwrap();
        assert_eq!(restored.conflicts, ["source.ts"]);
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user write before native response\n"
        );
    }
    #[test]
    fn checkpoint_conditional_restore_rechecks_under_the_shared_document_lease() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let relative = "source.ts";
        std::fs::write(root.join(relative), "Agent edit\n").unwrap();
        let expected = live_hash_for_path(&root, relative).unwrap();
        let baseline = BaselineFile {
            path: relative.into(),
            existed: true,
            content_base64: Some(STANDARD.encode(b"initial\n")),
            hash: Some(hash_bytes(b"initial\n")),
            mode: Some("100644".into()),
        };
        let lease = crate::coding_workspace::document_write_lock(&root.join(relative)).unwrap();
        let guard = lease.lock().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let ready = barrier.clone();
        let restore_root = root.clone();
        let worker = std::thread::spawn(move || {
            ready.wait();
            restore_one_if_current(&restore_root, &baseline, &expected)
        });
        barrier.wait();
        std::fs::write(
            root.join(relative),
            "user edit while native restore queued\n",
        )
        .unwrap();
        drop(guard);
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.contains("恢复冲突"));
        assert_eq!(
            std::fs::read_to_string(root.join(relative)).unwrap(),
            "user edit while native restore queued\n"
        );
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".coding.tmp")));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn readonly_git_baseline_and_sync_never_execute_configured_programs_or_refresh_index() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        init_git(root, &[("source.ts", "initial\n")]);
        let script = root.join(".git/read-sentinel");
        std::fs::write(&script, "#!/bin/sh\n: > \"$0.called\"\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for key in [
            "core.fsmonitor",
            "diff.external",
            "diff.fixture.textconv",
            "filter.fixture.clean",
            "filter.fixture.process",
        ] {
            let result = std::process::Command::new("git")
                .args(["config", key, &script.to_string_lossy()])
                .current_dir(root)
                .output()
                .unwrap();
            assert!(result.status.success());
        }
        std::fs::write(
            root.join(".git/info/attributes"),
            "*.ts diff=fixture filter=fixture\n",
        )
        .unwrap();
        let index = std::fs::read(root.join(".git/index")).unwrap();
        capture_baseline(root, "readonly-native", Vec::new()).unwrap();
        std::fs::write(root.join("source.ts"), "changed\n").unwrap();
        let synced = sync_from_git(root, "readonly-native").await.unwrap();
        assert!(synced
            .changes
            .iter()
            .any(|change| change.path == "source.ts"));
        let snapshot = crate::coding_workspace::git_snapshot(root).await;
        assert!(snapshot.has_git);
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        assert!(!root.join(".git/read-sentinel.called").exists(), "readonly baseline/snapshot ran a configured fsmonitor, external diff or textconv command");
    }
    #[test]
    fn whole_task_rollback_refuses_unknown_user_changes_before_touching_agent_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("agent.txt"), "initial Agent file\n").unwrap();
        std::fs::write(root.join("user.txt"), "initial user file\n").unwrap();
        let coding_task =
            task::create_task(root, "Safe cumulative rollback", "edit Agent file").unwrap();
        capture_filesystem_baseline(root, &coding_task.id).unwrap();
        checkpoint_start(root, &coding_task.id, "owned-run").unwrap();
        std::fs::write(root.join("agent.txt"), "confirmed Agent edit\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "owned-run", "agent.txt").unwrap();
        std::fs::write(root.join("user.txt"), "user typed during the run\n").unwrap();
        checkpoint_finish(root, &coding_task.id, "owned-run", &[]).unwrap();
        sync_from_filesystem(root, &coding_task.id).unwrap();
        let error = rollback_agent_owned(root, &coding_task.id).unwrap_err();
        assert!(
            error.contains("user.txt") && error.contains("本轮恢复"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("user.txt")).unwrap(),
            "user typed during the run\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("agent.txt")).unwrap(),
            "confirmed Agent edit\n",
            "reject the entire proposal before any restore"
        );
    }

    #[test]
    fn whole_task_rollback_allows_a_continuous_chain_of_confirmed_agent_edits() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "user starting content\n").unwrap();
        let coding_task =
            task::create_task(root, "Confirmed cumulative rollback", "edit source").unwrap();
        capture_filesystem_baseline(root, &coding_task.id).unwrap();
        for (run, content) in [
            ("pure-one", "first Agent edit\n"),
            ("pure-two", "second Agent edit\n"),
        ] {
            checkpoint_start(root, &coding_task.id, run).unwrap();
            std::fs::write(root.join("source.ts"), content).unwrap();
            checkpoint_note_applied(root, &coding_task.id, run, "source.ts").unwrap();
            checkpoint_finish(root, &coding_task.id, run, &[]).unwrap();
        }
        sync_from_filesystem(root, &coding_task.id).unwrap();
        assert_eq!(
            rollback_agent_owned(root, &coding_task.id).unwrap(),
            ["source.ts"]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user starting content\n"
        );
        assert!(load(root, &coding_task.id).changes.is_empty());
        checkpoint_start(root, &coding_task.id, "pure-after-rollback").unwrap();
        std::fs::write(
            root.join("source.ts"),
            "Agent edit after a successful rollback\n",
        )
        .unwrap();
        checkpoint_note_applied(root, &coding_task.id, "pure-after-rollback", "source.ts").unwrap();
        checkpoint_finish(root, &coding_task.id, "pure-after-rollback", &[]).unwrap();
        sync_from_filesystem(root, &coding_task.id).unwrap();
        assert_eq!(
            rollback_agent_owned(root, &coding_task.id).unwrap(),
            ["source.ts"]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "user starting content\n"
        );
    }

    #[test]
    fn whole_task_rollback_rejects_a_user_edit_between_two_confirmed_rounds() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "starting content\n").unwrap();
        let coding_task = task::create_task(root, "Inter-round ownership", "edit source").unwrap();
        capture_filesystem_baseline(root, &coding_task.id).unwrap();
        checkpoint_start(root, &coding_task.id, "before-user").unwrap();
        std::fs::write(root.join("source.ts"), "first Agent edit\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "before-user", "source.ts").unwrap();
        checkpoint_finish(root, &coding_task.id, "before-user", &[]).unwrap();
        std::fs::write(root.join("source.ts"), "user changed between rounds\n").unwrap();
        checkpoint_start(root, &coding_task.id, "after-user").unwrap();
        std::fs::write(
            root.join("source.ts"),
            "Agent preserved the new user content\n",
        )
        .unwrap();
        checkpoint_note_applied(root, &coding_task.id, "after-user", "source.ts").unwrap();
        checkpoint_finish(root, &coding_task.id, "after-user", &[]).unwrap();
        sync_from_filesystem(root, &coding_task.id).unwrap();
        assert!(rollback_agent_owned(root, &coding_task.id)
            .unwrap_err()
            .contains("交错"));
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "Agent preserved the new user content\n"
        );
    }

    #[test]
    fn late_external_terminal_observation_preserves_receipts_and_never_touches_a_new_turn() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "original\n").unwrap();
        std::fs::write(root.join("unchanged.ts"), "unchanged\n").unwrap();
        let mut coding_task =
            task::create_task(root, "Late MCP settlement", "edit source").unwrap();
        capture_filesystem_baseline(root, &coding_task.id).unwrap();
        checkpoint_start(root, &coding_task.id, "cancelled-run").unwrap();
        std::fs::write(root.join("source.ts"), "confirmed Agent content\n").unwrap();
        checkpoint_note_applied(root, &coding_task.id, "cancelled-run", "source.ts").unwrap();
        checkpoint_finish(root, &coding_task.id, "cancelled-run", &[]).unwrap();
        let old_path = checkpoint_path(root, &coding_task.id, "cancelled-run").unwrap();
        let original = store::read_json::<RunCheckpoint>(&old_path).unwrap();
        checkpoint_start(root, &coding_task.id, "replacement-run").unwrap();
        let replacement_path = checkpoint_path(root, &coding_task.id, "replacement-run").unwrap();
        let replacement_bytes = std::fs::read(&replacement_path).unwrap();

        std::fs::write(root.join("source.ts"), "late external server write\n").unwrap();
        std::fs::write(root.join("mcp.txt"), "external server created this\n").unwrap();
        checkpoint_observe_external_terminal(root, &coding_task.id, "cancelled-run").unwrap();
        let observed = store::read_json::<RunCheckpoint>(&old_path).unwrap();
        assert_eq!(observed.finished_at, original.finished_at);
        assert_eq!(observed.confirmed_paths, original.confirmed_paths);
        assert_eq!(observed.operation_receipts, original.operation_receipts);
        assert_eq!(observed.after_hashes, original.after_hashes);
        assert_eq!(observed.external_paths, ["mcp.txt", "source.ts"]);
        assert_eq!(std::fs::read(&replacement_path).unwrap(), replacement_bytes);
        let observed_bytes = std::fs::read(&old_path).unwrap();
        checkpoint_observe_external_terminal(root, &coding_task.id, "cancelled-run").unwrap();
        assert_eq!(std::fs::read(&old_path).unwrap(), observed_bytes);

        // Even a later return to a known Agent hash cannot convert the late
        // external write into an owned operation or authorize its restoration.
        std::fs::write(root.join("source.ts"), "confirmed Agent content\n").unwrap();
        checkpoint_observe_external_terminal(root, &coding_task.id, "cancelled-run").unwrap();
        coding_task.phase = TaskPhase::Paused;
        task::save(root, &coding_task).unwrap();
        let restored = checkpoint_restore(root, &coding_task.id, "cancelled-run").unwrap();
        assert!(restored.restored_paths.is_empty());
        assert!(restored.conflicts.contains(&"source.ts".into()));
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "confirmed Agent content\n"
        );
        assert!(root.join("mcp.txt").exists());
        assert_eq!(std::fs::read(&replacement_path).unwrap(), replacement_bytes);
    }

    #[test]
    fn text_discard_proposal_distinguishes_content_from_creation_deletion_binary_and_modes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "original\n").unwrap();
        capture_filesystem_baseline(root, "text-discard").unwrap();
        std::fs::write(root.join("source.ts"), "changed\n").unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        assert!(
            change_diff(root, "text-discard", "source.ts")
                .unwrap()
                .can_apply_text
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("source.ts"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            sync_from_filesystem(root, "text-discard").unwrap();
            assert!(
                !change_diff(root, "text-discard", "source.ts")
                    .unwrap()
                    .can_apply_text,
                "a text replacement cannot undo the simultaneous mode change"
            );
            std::fs::write(root.join("source.ts"), "original\n").unwrap();
            sync_from_filesystem(root, "text-discard").unwrap();
            assert!(
                !change_diff(root, "text-discard", "source.ts")
                    .unwrap()
                    .can_apply_text,
                "mode-only changes need the metadata operation"
            );
            std::fs::set_permissions(
                root.join("source.ts"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        std::fs::write(root.join("source.ts"), "\u{feff}changed UTF-8 BOM\n").unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        assert!(
            !change_diff(root, "text-discard", "source.ts")
                .unwrap()
                .can_apply_text,
            "BOM is encoding metadata and must be restored through the byte-preserving path"
        );
        std::fs::write(root.join("source.ts"), "a".repeat(2_000_001)).unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        assert!(
            !change_diff(root, "text-discard", "source.ts")
                .unwrap()
                .can_apply_text,
            "the document bridge must never receive an oversized replacement"
        );
        std::fs::write(root.join("source.ts"), b"binary\0data").unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        let binary = change_diff(root, "text-discard", "source.ts").unwrap();
        assert!(binary.binary && !binary.can_apply_text);
        std::fs::remove_file(root.join("source.ts")).unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        assert!(
            !change_diff(root, "text-discard", "source.ts")
                .unwrap()
                .can_apply_text
        );
        std::fs::write(root.join("added.ts"), "new file\n").unwrap();
        sync_from_filesystem(root, "text-discard").unwrap();
        assert!(
            !change_diff(root, "text-discard", "added.ts")
                .unwrap()
                .can_apply_text
        );
    }
    #[test]
    fn a_renderer_filename_without_native_content_evidence_never_grants_restore() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "starting content").unwrap();
        let task = task::create_task(root, "Receipt protection", "edit source").unwrap();
        checkpoint_start(root, &task.id, "failed-save").unwrap();
        std::fs::write(root.join("source.ts"), "external user content").unwrap();
        let finished =
            checkpoint_finish(root, &task.id, "failed-save", &["source.ts".into()]).unwrap();
        assert!(finished.confirmed_paths.is_empty());
        assert_eq!(finished.external_paths, ["source.ts"]);
        assert!(checkpoint_restore(root, &task.id, "failed-save")
            .unwrap()
            .restored_paths
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "external user content"
        );
    }
    #[test]
    fn native_run_mode_is_immutable_and_unknown_authority_never_schedules_checks() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let task = task::create_task(root, "Ask", "explain").unwrap();
        checkpoint_bind_mode(root, &task.id, "ask-run", Some("ask")).unwrap();
        assert!(checkpoint_run_is_readonly(root, &task.id, "ask-run").unwrap());
        assert!(checkpoint_bind_mode(root, &task.id, "ask-run", Some("agent")).is_err());
        checkpoint_bind_mode(root, &task.id, "agent-run", Some("agent")).unwrap();
        assert!(!checkpoint_run_is_readonly(root, &task.id, "agent-run").unwrap());
        checkpoint_bind_mode(root, &task.id, "legacy-run", None).unwrap();
        assert!(checkpoint_run_is_readonly(root, &task.id, "legacy-run").unwrap());
    }
}
