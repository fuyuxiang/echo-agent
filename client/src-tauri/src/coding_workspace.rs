//! Native services used by the dedicated Coding Workspace.
//!
//! The renderer may only inspect or execute inside a directory that was
//! granted by the native folder picker/session catalog.  The implementation is
//! intentionally dependency-light: repository discovery is bounded and build
//! commands run in a time-boxed child shell with capped output.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine as _;
use echo_agent_pty::pty::{PtyChild, PtyConfig, PtyHandle, PtyMaster};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
#[cfg(test)]
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::shell_fs::FilesystemAccess;

const MAX_SCANNED_FILES: usize = 12_000;
const MAX_SCAN_DEPTH: usize = 18;
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
// SP5: 4 MB → 2 MB. Anything bigger should not be loaded into the editor.
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_GIT_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 300;
const MAX_SEARCH_PREVIEW_CHARS: usize = 600;
/// SP1: maximum number of entries accepted by a single batch FS-mutation call.
const MAX_BATCH_OPS: usize = 500;
#[cfg(target_os = "macos")]
const MAX_TRASH_RESTORE_TOKENS: usize = 25_000;
/// How often a long workspace scan reports progress to the UI.
const PROGRESS_EVERY_FILES: usize = 2_000;

const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".idea",
    ".vscode",
    "node_modules",
    "target",
    "dist",
    "build",
    "coverage",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingLanguageStat {
    language: String,
    files: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingModule {
    name: String,
    path: String,
    kind: String,
    dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingWorkspaceAnalysis {
    root: String,
    name: String,
    project_type: String,
    file_count: usize,
    truncated: bool,
    languages: Vec<CodingLanguageStat>,
    modules: Vec<CodingModule>,
    validation_commands: Vec<String>,
    has_git: bool,
    git_branch: Option<String>,
    git_changed_files: usize,
    instruction_files: Vec<String>,
    scanned_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingGitFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingGitSnapshot {
    pub(crate) has_git: bool,
    branch: Option<String>,
    pub(crate) head: Option<String>,
    pub files: Vec<CodingGitFile>,
    total_added: usize,
    total_removed: usize,
    captured_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingDocument {
    path: String,
    relative_path: String,
    content: String,
    hash: String,
    size: usize,
    modified_at: u128,
    language: String,
    line_ending: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSearchHit {
    path: String,
    line: usize,
    column: usize,
    preview: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSearchOptions {
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    whole_word: bool,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    include_glob: String,
    #[serde(default)]
    exclude_glob: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingWriteDocumentRequest {
    root: String,
    path: String,
    content: String,
    expected_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingCreateEntryRequest {
    root: String,
    parent: Option<String>,
    name: String,
    directory: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingDeleteEntryRequest {
    pub root: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingRenameEntryRequest {
    pub root: String,
    pub path: String,
    pub new_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingCopyEntryRequest {
    pub root: String,
    pub sources: Vec<String>,
    pub destination: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingMoveEntryRequest {
    pub root: String,
    pub sources: Vec<String>,
    pub destination: String,
}

/// SP5: counterpart to `CodingDeleteEntryRequest` used to undo a deletion.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingRestoreRequest {
    pub root: String,
    pub original_paths: Vec<String>,
    pub restore_tokens: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingBatchOpResult {
    /// The resulting path on success; the source path on failure.
    pub path: String,
    /// Original source path, allowing callers to correlate partial batches.
    pub source_path: String,
    pub ok: bool,
    pub error: Option<String>,
    /// Opaque token required to restore an item moved to the system trash.
    pub restore_token: Option<String>,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct TrashRestoreRecord {
    original_path: PathBuf,
    trashed_path: PathBuf,
}

#[cfg(target_os = "macos")]
static TRASH_RESTORE_RECORDS: OnceLock<Mutex<HashMap<String, TrashRestoreRecord>>> =
    OnceLock::new();

#[cfg(target_os = "macos")]
fn trash_restore_records() -> &'static Mutex<HashMap<String, TrashRestoreRecord>> {
    TRASH_RESTORE_RECORDS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingRenameResult {
    pub path: String,
    pub old_path: String,
}

#[derive(Default)]
pub struct CodingProcesses {
    commands: Mutex<HashMap<String, CancellationToken>>,
    terminals: Mutex<HashMap<String, Arc<CodingTerminalSession>>>,
    verification_approvals: Mutex<HashMap<String, VerificationApproval>>,
}

struct VerificationApproval {
    root: PathBuf,
    task_id: String,
    command: String,
    expires_at: Instant,
}

impl CodingProcesses {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track a running command so it can be cancelled by id. Shared with the
    /// verification engine, which is the single owner of command execution.
    pub fn register(&self, run_id: &str, token: CancellationToken) -> Result<(), String> {
        let mut commands = self
            .commands
            .lock()
            .map_err(|_| "命令运行状态已损坏".to_string())?;
        if commands.contains_key(run_id) {
            return Err("命令运行标识已存在".into());
        }
        commands.insert(run_id.to_string(), token);
        Ok(())
    }

    pub fn unregister(&self, run_id: &str) {
        if let Ok(mut commands) = self.commands.lock() {
            commands.remove(run_id);
        }
    }

    pub fn cancel(&self, run_id: &str) -> Result<(), String> {
        let commands = self
            .commands
            .lock()
            .map_err(|_| "命令运行状态已损坏".to_string())?;
        match commands.get(run_id) {
            Some(token) => {
                token.cancel();
                Ok(())
            }
            None => Err("命令已结束或不存在".into()),
        }
    }

    /// Create a short-lived, one-shot grant after the user confirms a plan
    /// command in the workbench. Automatic manifest commands do not need one.
    pub fn approve_verification(
        &self,
        root: &Path,
        task_id: &str,
        command: &str,
    ) -> Result<String, String> {
        let token = uuid::Uuid::now_v7().to_string();
        let mut approvals = self
            .verification_approvals
            .lock()
            .map_err(|_| "验证授权状态已损坏".to_string())?;
        approvals.retain(|_, approval| approval.expires_at > Instant::now());
        if approvals.len() >= 256 {
            return Err("待运行的验证授权过多，请稍后重试".into());
        }
        approvals.insert(
            token.clone(),
            VerificationApproval {
                root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
                task_id: task_id.to_string(),
                command: command.to_string(),
                expires_at: Instant::now() + Duration::from_secs(5 * 60),
            },
        );
        Ok(token)
    }

    pub fn consume_verification_approval(
        &self,
        token: &str,
        root: &Path,
        task_id: &str,
        command: &str,
    ) -> Result<(), String> {
        let mut approvals = self
            .verification_approvals
            .lock()
            .map_err(|_| "验证授权状态已损坏".to_string())?;
        approvals.retain(|_, approval| approval.expires_at > Instant::now());
        let approval = approvals
            .remove(token)
            .ok_or_else(|| "自定义验证命令尚未确认或授权已过期".to_string())?;
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if approval.root != canonical || approval.task_id != task_id || approval.command != command
        {
            return Err("自定义验证授权与当前任务或命令不匹配".into());
        }
        Ok(())
    }
}

struct CodingTerminalSession {
    master: PtyMaster,
    child: Mutex<PtyChild>,
    writer: Mutex<Box<dyn Write + Send>>,
}

fn terminate_terminal_child(child: &mut PtyChild) -> Result<(), String> {
    if !child.is_alive() {
        return Ok(());
    }
    let pid = child.pid();
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Shells spawned by portable-pty own the foreground PTY process group.
        // Stopping the group prevents a compiler/test child from surviving a
        // terminal restart or workspace switch.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
        if child.is_alive() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(pid) = pid {
        let mut kill = crate::process_supervisor::background_sync_command("taskkill");
        kill.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = kill.status();
    }
    if child.is_alive() {
        child
            .kill()
            .map_err(|error| format!("关闭终端失败：{error}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodingTerminalEvent {
    terminal_id: String,
    data_base64: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingTerminalCreated {
    terminal_id: String,
}

#[derive(Debug, Clone)]
struct ManifestCandidate {
    path: PathBuf,
    kind: &'static str,
}

fn bounded_text(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn language_for_extension(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "ts" | "tsx" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "py" => Some("Python"),
        "rs" => Some("Rust"),
        "go" => Some("Go"),
        "cs" => Some("C#"),
        "c" | "h" => Some("C/C++"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("C/C++"),
        "vue" => Some("Vue"),
        "svelte" => Some("Svelte"),
        "php" => Some("PHP"),
        "rb" => Some("Ruby"),
        "sql" => Some("SQL"),
        "html" | "htm" => Some("HTML"),
        "css" | "scss" | "less" => Some("CSS"),
        _ => None,
    }
}

fn manifest_kind(name: &str) -> Option<&'static str> {
    match name {
        "pom.xml" => Some("Maven"),
        "build.gradle" | "build.gradle.kts" => Some("Gradle"),
        "package.json" => Some("Node.js"),
        "pyproject.toml" | "requirements.txt" => Some("Python"),
        "Cargo.toml" => Some("Rust"),
        "go.mod" => Some("Go"),
        "build.sbt" => Some("Scala"),
        _ => None,
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn first_tag(text: &str, tag: &str) -> Option<String> {
    let start_marker = format!("<{tag}>");
    let end_marker = format!("</{tag}>");
    let start = text.find(&start_marker)? + start_marker.len();
    let rest = &text[start..];
    let end = rest.find(&end_marker)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn quoted_toml_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let (left, right) = line.split_once('=')?;
        if left.trim() != key {
            return None;
        }
        let value = right
            .trim()
            .trim_matches(|character| character == '"' || character == '\'');
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn module_name(manifest: &ManifestCandidate, module_root: &Path) -> String {
    let fallback = module_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace")
        .to_string();
    let Some(text) = bounded_text(&manifest.path) else {
        return fallback;
    };
    match manifest.kind {
        "Node.js" => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("name")?.as_str().map(str::to_string))
            .unwrap_or(fallback),
        "Maven" => first_tag(&text, "artifactId").unwrap_or(fallback),
        "Rust" | "Python" => quoted_toml_value(&text, "name").unwrap_or(fallback),
        "Go" => text
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("module ")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback),
        _ => fallback,
    }
}

fn validation_commands(root: &Path, manifests: &[ManifestCandidate]) -> Vec<String> {
    let mut commands = Vec::new();
    for manifest in manifests {
        let Some(module_root) = manifest.path.parent() else {
            continue;
        };
        let relative_root = relative_display(root, module_root);
        let at_root = module_root == root;
        let quoted_root = format!("\"{}\"", relative_root.replace('"', "\\\""));
        match manifest.kind {
            "Maven" => {
                let wrapper = module_root.join(if cfg!(target_os = "windows") {
                    "mvnw.cmd"
                } else {
                    "mvnw"
                });
                if wrapper.is_file() {
                    commands.push(if at_root {
                        if cfg!(target_os = "windows") {
                            "mvnw.cmd test".into()
                        } else {
                            "./mvnw test".into()
                        }
                    } else {
                        format!("\"{}\" test", relative_display(root, &wrapper))
                    });
                } else if at_root {
                    commands.push("mvn test".into());
                } else {
                    commands.push(format!(
                        "mvn -f \"{}\" test",
                        relative_display(root, &manifest.path)
                    ));
                }
            }
            "Gradle" => {
                let wrapper = module_root.join(if cfg!(target_os = "windows") {
                    "gradlew.bat"
                } else {
                    "gradlew"
                });
                if wrapper.is_file() {
                    commands.push(if at_root {
                        if cfg!(target_os = "windows") {
                            "gradlew.bat test".into()
                        } else {
                            "./gradlew test".into()
                        }
                    } else {
                        format!("\"{}\" test", relative_display(root, &wrapper))
                    });
                } else if at_root {
                    commands.push("gradle test".into());
                } else {
                    commands.push(format!("gradle -p {quoted_root} test"));
                }
            }
            "Node.js" => {
                let scripts = bounded_text(&manifest.path)
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .and_then(|value| value.get("scripts").cloned())
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                let manager = if root.join("pnpm-lock.yaml").is_file()
                    || module_root.join("pnpm-lock.yaml").is_file()
                {
                    "pnpm"
                } else if root.join("yarn.lock").is_file()
                    || module_root.join("yarn.lock").is_file()
                {
                    "yarn"
                } else {
                    "npm"
                };
                for script in ["test", "build", "lint", "typecheck"] {
                    if !scripts.contains_key(script) {
                        continue;
                    }
                    commands.push(match (manager, at_root, script) {
                        ("pnpm", true, _) => format!("pnpm {script}"),
                        ("pnpm", false, _) => format!("pnpm --dir {quoted_root} {script}"),
                        ("yarn", true, _) => format!("yarn {script}"),
                        ("yarn", false, _) => format!("yarn --cwd {quoted_root} {script}"),
                        ("npm", true, "test") => "npm test".into(),
                        ("npm", true, _) => format!("npm run {script}"),
                        ("npm", false, "test") => format!("npm --prefix {quoted_root} test"),
                        ("npm", false, _) => format!("npm --prefix {quoted_root} run {script}"),
                        _ => unreachable!(),
                    });
                }
            }
            "Python" => commands.push(if at_root {
                "pytest".into()
            } else {
                format!("pytest {quoted_root}")
            }),
            "Rust" => commands.push(if at_root {
                "cargo test".into()
            } else {
                format!(
                    "cargo test --manifest-path \"{}\"",
                    relative_display(root, &manifest.path)
                )
            }),
            "Go" => commands.push(if at_root {
                "go test ./...".into()
            } else {
                format!("go -C {quoted_root} test ./...")
            }),
            _ => {}
        }
    }
    commands.sort();
    commands.dedup();
    commands.truncate(24);
    commands
}

async fn git_output(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = crate::process_supervisor::background_async_command("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn git_output_bytes(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = crate::process_supervisor::background_async_command("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn git_status_label(x: u8, y: u8) -> &'static str {
    if x == b'?' && y == b'?' {
        "untracked"
    } else if x == b'!' && y == b'!' {
        "ignored"
    } else if matches!(x, b'U') || matches!(y, b'U') || (x == b'A' && y == b'A') {
        "conflict"
    } else if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
        "renamed"
    } else if matches!(x, b'D') || matches!(y, b'D') {
        "deleted"
    } else if matches!(x, b'A') || matches!(y, b'A') {
        "added"
    } else {
        "modified"
    }
}

fn parse_git_numstat(output: &[u8]) -> HashMap<String, (usize, usize)> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            let mut columns = line.splitn(3, '\t');
            let added = columns.next()?.parse::<usize>().unwrap_or_default();
            let removed = columns.next()?.parse::<usize>().unwrap_or_default();
            let path = columns.next()?.to_string();
            Some((path, (added, removed)))
        })
        .collect()
}

fn parse_git_status(output: &[u8], stats: &HashMap<String, (usize, usize)>) -> Vec<CodingGitFile> {
    let mut entries = output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty());
    let mut files = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let x = entry[0];
        let y = entry[1];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let old_path = if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            entries
                .next()
                .map(|value| String::from_utf8_lossy(value).into_owned())
        } else {
            None
        };
        let (added, removed) = stats.get(&path).copied().unwrap_or_default();
        files.push(CodingGitFile {
            path,
            old_path,
            status: git_status_label(x, y).to_string(),
            staged: x != b' ' && x != b'?',
            unstaged: y != b' ' && y != b'?',
            untracked: x == b'?' && y == b'?',
            added,
            removed,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

/// Public so the changeset module can derive a per-task change set from the
/// live Git state after the Agent finishes.
pub async fn git_snapshot(root: &Path) -> CodingGitSnapshot {
    let has_git = git_output(root, &["rev-parse", "--is-inside-work-tree"])
        .await
        .as_deref()
        == Some("true");
    if !has_git {
        return CodingGitSnapshot {
            has_git: false,
            branch: None,
            head: None,
            files: Vec::new(),
            total_added: 0,
            total_removed: 0,
            captured_at: chrono::Utc::now().to_rfc3339(),
        };
    }
    let status = git_output_bytes(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )
    .await
    .unwrap_or_default();
    let numstat = git_output_bytes(root, &["diff", "--numstat", "HEAD", "--"])
        .await
        .unwrap_or_default();
    let stats = parse_git_numstat(&numstat);
    let files = parse_git_status(&status, &stats);
    CodingGitSnapshot {
        has_git: true,
        branch: git_output(root, &["branch", "--show-current"])
            .await
            .filter(|value| !value.is_empty()),
        head: git_output(root, &["rev-parse", "--short=10", "HEAD"])
            .await
            .filter(|value| !value.is_empty()),
        total_added: files.iter().map(|file| file.added).sum(),
        total_removed: files.iter().map(|file| file.removed).sum(),
        files,
        captured_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[tauri::command]
pub async fn coding_git_snapshot(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<CodingGitSnapshot, String> {
    let root = access.require_workspace(&root)?;
    Ok(git_snapshot(&root).await)
}

#[tauri::command]
pub async fn coding_git_diff(
    access: State<'_, FilesystemAccess>,
    root: String,
    path: Option<String>,
) -> Result<String, String> {
    let root = access.require_workspace(&root)?;
    let mut arguments = vec![
        "diff",
        "--no-ext-diff",
        "--no-color",
        "--unified=3",
        "HEAD",
        "--",
    ];
    let normalized_path = path
        .as_deref()
        .map(|value| normalize_git_relative_path(&root, value))
        .transpose()?;
    if let Some(value) = normalized_path.as_deref() {
        arguments.push(value);
    }
    let output = git_output_bytes(&root, &arguments)
        .await
        .unwrap_or_default();
    let mut truncated = output.len() > MAX_GIT_DIFF_BYTES;
    let mut text =
        String::from_utf8_lossy(&output[..output.len().min(MAX_GIT_DIFF_BYTES)]).into_owned();
    if text.is_empty() {
        if let Some(path) = normalized_path.as_deref() {
            let tracked = git_output(&root, &["ls-files", "--error-unmatch", "--", path])
                .await
                .is_some();
            let candidate = root.join(path);
            if !tracked && candidate.is_file() {
                let safe = resolve_coding_document_path(&root, path)?;
                let bytes = tokio::fs::read(&safe)
                    .await
                    .map_err(|error| format!("无法读取未跟踪文件：{error}"))?;
                truncated = bytes.len() > MAX_GIT_DIFF_BYTES;
                if bytes.contains(&0) {
                    text =
                        format!("Binary file {path} is untracked; binary diff is not displayed.\n");
                } else {
                    let visible =
                        String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_GIT_DIFF_BYTES)]);
                    let line_count = visible.lines().count();
                    text = format!(
                        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{line_count} @@\n{}",
                        visible
                            .lines()
                            .map(|line| format!("+{line}\n"))
                            .collect::<String>()
                    );
                }
            }
        }
    }
    if truncated {
        text.push_str("\n…Diff 已达到 2 MB 安全上限，后续内容省略…\n");
    }
    Ok(text)
}

#[tauri::command]
pub async fn coding_git_set_staged(
    access: State<'_, FilesystemAccess>,
    root: String,
    path: String,
    staged: bool,
) -> Result<CodingGitSnapshot, String> {
    let root = access.require_workspace(&root)?;
    let path = normalize_git_relative_path(&root, &path)?;
    if git_output(&root, &["rev-parse", "--is-inside-work-tree"])
        .await
        .as_deref()
        != Some("true")
    {
        return Err("当前工作区不是 Git 仓库".into());
    }
    let arguments = if staged {
        vec!["add", "--", path.as_str()]
    } else {
        vec!["restore", "--staged", "--", path.as_str()]
    };
    let mut output = crate::process_supervisor::background_async_command("git")
        .arg("-C")
        .arg(&root)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| format!("无法执行 Git 操作：{error}"))?;
    // `git restore --staged` requires a valid HEAD. Fresh repositories do not
    // have one yet, so fall back to removing only the index entry while keeping
    // the working-tree file intact.
    if !staged && !output.status.success() {
        output = crate::process_supervisor::background_async_command("git")
            .arg("-C")
            .arg(&root)
            .args(["rm", "--cached", "--ignore-unmatch", "--", path.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| format!("无法执行 Git 操作：{error}"))?;
    }
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{}失败：{}",
            if staged {
                "暂存文件"
            } else {
                "取消暂存"
            },
            detail.trim().chars().take(1_000).collect::<String>()
        ));
    }
    Ok(git_snapshot(&root).await)
}

fn normalize_git_relative_path(root: &Path, claimed: &str) -> Result<String, String> {
    let raw = PathBuf::from(claimed);
    let relative = if raw.is_absolute() {
        raw.strip_prefix(root)
            .map_err(|_| "Git Diff 路径不在当前工作区内".to_string())?
            .to_path_buf()
    } else {
        raw
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err("Git Diff 路径包含不安全的目录跳转".into());
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn resolve_coding_document_path(root: &Path, claimed: &str) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(claimed);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    };
    let metadata =
        std::fs::symlink_metadata(&candidate).map_err(|error| format!("无法读取文件：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("代码编辑器只能打开工作区内的普通文件".into());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("无法解析文件：{error}"))?;
    if !canonical.starts_with(root) {
        return Err("拒绝访问工作区之外的文件".into());
    }
    Ok(canonical)
}

fn document_hash(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn editor_language(path: &Path) -> String {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        // Monaco uses the same language id for TS/TSX (and JS/JSX); JSX mode
        // is selected from the model URI extension by its language service.
        "ts" | "tsx" => "typescript",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "rs" => "rust",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "json" => "json",
        "md" | "mdx" => "markdown",
        "xml" | "svg" => "xml",
        "yaml" | "yml" => "yaml",
        "toml" => "ini",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        _ => "plaintext",
    }
    .to_string()
}

fn read_coding_document(root: &Path, claimed: &str) -> Result<CodingDocument, String> {
    let path = resolve_coding_document_path(root, claimed)?;
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("无法读取文件信息：{error}"))?;
    if metadata.len() as usize > MAX_DOCUMENT_BYTES {
        return Err("文件超过 2 MB，为避免编辑器卡顿已拒绝打开".into());
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("读取文件失败：{error}"))?;
    if bytes.contains(&0) {
        return Err("二进制文件不能在代码编辑器中打开".into());
    }
    let content = String::from_utf8(bytes.clone())
        .map_err(|_| "文件不是 UTF-8 编码，暂不支持编辑".to_string())?;
    Ok(CodingDocument {
        path: path.to_string_lossy().into_owned(),
        relative_path: relative_display(root, &path),
        hash: document_hash(&bytes),
        size: bytes.len(),
        modified_at: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis())
            .unwrap_or_default(),
        language: editor_language(&path),
        line_ending: if content.contains("\r\n") {
            "CRLF"
        } else {
            "LF"
        }
        .to_string(),
        content,
    })
}

#[tauri::command]
pub async fn coding_read_document(
    access: State<'_, FilesystemAccess>,
    root: String,
    path: String,
) -> Result<CodingDocument, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || read_coding_document(&root, &path))
        .await
        .map_err(|error| format!("读取文件失败：{error}"))?
}

#[tauri::command]
pub async fn coding_write_document(
    access: State<'_, FilesystemAccess>,
    request: CodingWriteDocumentRequest,
) -> Result<CodingDocument, String> {
    if request.content.len() > MAX_DOCUMENT_BYTES {
        return Err("写入内容超过 2 MB 安全上限".into());
    }
    let root = access.require_workspace(&request.root)?;
    // The staged write, permission copy and atomic replace form one synchronous
    // unit; keep them together on the blocking pool.
    tokio::task::spawn_blocking(move || write_coding_document_blocking(&root, request))
        .await
        .map_err(|error| format!("保存文件失败：{error}"))?
}

fn write_coding_document_blocking(
    root: &Path,
    request: CodingWriteDocumentRequest,
) -> Result<CodingDocument, String> {
    let path = resolve_coding_document_path(root, &request.path)?;
    let current = std::fs::read(&path).map_err(|error| format!("保存前无法读取文件：{error}"))?;
    if document_hash(&current) != request.expected_hash {
        return Err("保存冲突：文件已被 Agent 或其他程序修改，请重新加载后合并改动".into());
    }
    let parent = path.parent().ok_or_else(|| "文件没有父目录".to_string())?;
    let name = path.file_name().ok_or_else(|| "文件名无效".to_string())?;
    let staging = parent.join(format!(
        ".{}.{}.{}.coding.tmp",
        name.to_string_lossy(),
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));
    let result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| format!("创建临时文件失败：{error}"))?;
        file.write_all(request.content.as_bytes())
            .map_err(|error| format!("写入临时文件失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步文件失败：{error}"))?;
        if let Ok(metadata) = std::fs::metadata(&path) {
            std::fs::set_permissions(&staging, metadata.permissions())
                .map_err(|error| format!("保留文件权限失败：{error}"))?;
        }
        drop(file);
        crate::paths::replace_file_atomically(&staging, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result?;
    read_coding_document(root, &path.to_string_lossy())
}

fn safe_new_entry_name(name: &str) -> Result<&str, String> {
    let value = name.trim();
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 240
        || value == "."
        || value == ".."
        || value.eq_ignore_ascii_case(".git")
        || value.contains('/')
        || value.contains('\\')
        || value
            .chars()
            .any(|character| character == '\0' || character.is_control())
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err("名称不能为空、不能包含目录跳转或路径分隔符，也不能创建 .git".into());
    }
    Ok(value)
}

fn resolve_coding_directory_path(root: &Path, claimed: Option<&str>) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("无法解析工作区：{error}"))?;
    let claimed = claimed.map(str::trim).filter(|value| !value.is_empty());
    let candidate = match claimed {
        Some(value) if Path::new(value).is_absolute() => PathBuf::from(value),
        Some(value) => canonical_root.join(value),
        None => canonical_root.clone(),
    };
    let metadata = std::fs::symlink_metadata(&candidate)
        .map_err(|error| format!("无法读取父目录：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("只能在工作区内的真实目录中创建文件".into());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("无法解析父目录：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err("拒绝在工作区之外创建文件".into());
    }
    Ok(canonical)
}

#[tauri::command]
pub async fn coding_create_entry(
    access: State<'_, FilesystemAccess>,
    request: CodingCreateEntryRequest,
) -> Result<String, String> {
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || create_entry_blocking(&root, request))
        .await
        .map_err(|error| format!("创建失败：{error}"))?
}

fn create_entry_blocking(root: &Path, request: CodingCreateEntryRequest) -> Result<String, String> {
    let parent = resolve_coding_directory_path(root, request.parent.as_deref())?;
    let name = safe_new_entry_name(&request.name)?;
    let target = parent.join(name);
    if target.exists() {
        return Err("同名文件或目录已经存在".into());
    }
    if request.directory {
        std::fs::create_dir(&target).map_err(|error| format!("创建目录失败：{error}"))?;
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("创建文件失败：{error}"))?;
    }
    Ok(target.to_string_lossy().into_owned())
}

// ----- SP1: file ops helpers (delete / rename / copy / move) -----

/// Resolve an existing entry (file or directory) inside `root`, rejecting
/// symlinks and entries outside the workspace. Returns the canonical path.
fn resolve_coding_path_strict(root: &Path, claimed: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("无法解析工作区：{error}"))?;
    let candidate = PathBuf::from(claimed);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        canonical_root.join(candidate)
    };
    let metadata =
        std::fs::symlink_metadata(&candidate).map_err(|error| format!("无法读取路径：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("拒绝访问符号链接".into());
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("无法解析路径：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err("拒绝访问工作区之外的路径".into());
    }
    Ok(canonical)
}

/// Recursively copy a directory tree (refuses to follow symlinks; preserves
/// permissions and best-effort mtime). Used for cross-volume move fallback.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        let ft = entry.file_type();
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("拒绝复制符号链接：{}", entry.path().display()),
            ));
        }
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        let target = dst.join(rel);
        if ft.is_dir() {
            std::fs::create_dir_all(&target)?;
            if let Ok(perm) = entry.metadata().map(|m| m.permissions()) {
                let _ = std::fs::set_permissions(&target, perm);
            }
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &target)?;
            if let Ok(meta) = entry.metadata() {
                let _ = std::fs::set_permissions(&target, meta.permissions());
            }
        }
    }
    Ok(())
}

fn copy_one(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "拒绝复制符号链接",
        ));
    }
    let result = (|| {
        if meta.is_dir() {
            copy_dir_recursive(src, dst)?;
        } else {
            std::fs::copy(src, dst)?;
            let _ = std::fs::set_permissions(dst, meta.permissions());
        }
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup = match std::fs::symlink_metadata(dst) {
            Ok(_) => fallback_remove(dst),
            Err(check_error) if check_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(check_error) => Err(format!("无法检查未完整副本：{check_error}")),
        };
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => std::io::Error::new(
                error.kind(),
                format!("{error}；且未完整副本清理失败：{cleanup_error}"),
            ),
        });
    }
    Ok(())
}

// ----- SP1: 4 new commands -----

#[tauri::command]
pub async fn coding_delete_entries(
    access: State<'_, FilesystemAccess>,
    request: CodingDeleteEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    if request.paths.is_empty() {
        return Ok(Vec::new());
    }
    if request.paths.len() > MAX_BATCH_OPS {
        return Err(format!("一次最多删除 {MAX_BATCH_OPS} 个条目"));
    }
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || delete_entries_blocking(&root, request))
        .await
        .map_err(|error| format!("删除失败：{error}"))?
}

fn delete_entries_blocking(
    root: &Path,
    request: CodingDeleteEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    // Pre-resolve every source; reject the whole batch on first escape attempt.
    let mut resolved = Vec::with_capacity(request.paths.len());
    let mut seen = HashSet::new();
    for raw in &request.paths {
        let canonical = resolve_coding_path_strict(root, raw)?;
        if !seen.insert(canonical.clone()) {
            return Err("删除批次包含重复路径".into());
        }
        resolved.push(canonical);
    }
    // Deletion is recoverable by contract. If the platform trash is
    // unavailable, report a per-item failure and leave the source untouched.
    let mut results: Vec<CodingBatchOpResult> = Vec::with_capacity(resolved.len());
    for path in &resolved {
        let display = path.to_string_lossy().into_owned();
        let outcome = trash_path(path);
        match outcome {
            Ok(restore_token) => results.push(CodingBatchOpResult {
                path: display.clone(),
                source_path: display,
                ok: true,
                error: None,
                restore_token: Some(restore_token),
            }),
            Err(error) => results.push(CodingBatchOpResult {
                path: display.clone(),
                source_path: display,
                ok: false,
                error: Some(error),
                restore_token: None,
            }),
        }
    }
    Ok(results)
}

fn trash_path(path: &Path) -> Result<String, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("无法读取路径：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("拒绝删除符号链接".into());
    }
    #[cfg(target_os = "macos")]
    {
        trash_path_macos(path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(path)
            .map_err(|error| format!("无法移到系统回收站，文件未被删除：{error}"))?;
        Ok(path.to_string_lossy().into_owned())
    }
}

#[cfg(target_os = "macos")]
fn trash_path_macos(path: &Path) -> Result<String, String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let path_string = path.to_string_lossy();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path_string));
    let manager = NSFileManager::defaultManager();
    let mut resulting_url = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting_url))
        .map_err(|error| format!("无法移到系统回收站，文件未被删除：{error}"))?;
    let trashed_path = resulting_url
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "条目已移到回收站，但系统未返回恢复位置".to_string())?;
    let token = uuid::Uuid::now_v7().to_string();
    let mut records = trash_restore_records()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if records.len() >= MAX_TRASH_RESTORE_TOKENS {
        if let Some(oldest) = records.keys().next().cloned() {
            records.remove(&oldest);
        }
    }
    records.insert(
        token.clone(),
        TrashRestoreRecord {
            original_path: path.to_path_buf(),
            trashed_path,
        },
    );
    Ok(token)
}

fn fallback_remove(path: &Path) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("无法读取路径：{error}"))?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path).map_err(|error| format!("删除目录失败：{error}"))
    } else {
        std::fs::remove_file(path).map_err(|error| format!("删除文件失败：{error}"))
    }
}

// ----- SP5: undo a previous `coding_delete_entries` by restoring the trashed entry. -----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingRestoreResult {
    pub path: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn coding_restore_from_trash(
    access: State<'_, FilesystemAccess>,
    request: CodingRestoreRequest,
) -> Result<Vec<CodingRestoreResult>, String> {
    if request.original_paths.len() != request.restore_tokens.len() {
        return Err("original_paths 与 restore_tokens 数量不匹配".into());
    }
    if request.original_paths.len() > MAX_BATCH_OPS {
        return Err(format!("一次最多恢复 {MAX_BATCH_OPS} 个条目"));
    }
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || restore_blocking(&root, request))
        .await
        .map_err(|error| format!("恢复失败：{error}"))?
}

fn restore_blocking(
    root: &Path,
    request: CodingRestoreRequest,
) -> Result<Vec<CodingRestoreResult>, String> {
    // Validate every destination before touching the trash. Otherwise a
    // predictable collision near the end of a batch would leave earlier
    // entries restored and the history action only half applied.
    let mut seen_targets = HashSet::new();
    let validations: Vec<Result<PathBuf, String>> = request
        .original_paths
        .iter()
        .map(|orig| {
            let target = resolve_coding_target_path(root, orig)?;
            if !seen_targets.insert(target.clone()) {
                return Err("恢复批次包含重复目标".into());
            }
            if target
                .try_exists()
                .map_err(|error| format!("无法检测恢复目标：{error}"))?
            {
                return Err("目标位置已存在同名文件或目录".into());
            }
            Ok(target)
        })
        .collect();
    if validations.iter().any(Result::is_err) {
        return Ok(request
            .original_paths
            .iter()
            .zip(validations)
            .map(|(orig, validation)| CodingRestoreResult {
                path: orig.clone(),
                ok: false,
                error: Some(
                    validation
                        .err()
                        .unwrap_or_else(|| "同批次包含无法恢复的条目，本次未执行任何恢复".into()),
                ),
            })
            .collect());
    }

    let targets = validations
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    if let Err(error) = preflight_restore_batch(&request.restore_tokens, &targets) {
        return Ok(request
            .original_paths
            .iter()
            .map(|orig| CodingRestoreResult {
                path: orig.clone(),
                ok: false,
                error: Some(format!("恢复批次未执行：{error}")),
            })
            .collect());
    }
    let mut out = Vec::with_capacity(request.original_paths.len());
    for ((orig, restore_token), target) in request
        .original_paths
        .iter()
        .zip(request.restore_tokens.iter())
        .zip(targets)
    {
        match restore_trash_item(restore_token, &target) {
            Ok(()) => out.push(CodingRestoreResult {
                path: target.to_string_lossy().into_owned(),
                ok: true,
                error: None,
            }),
            Err(e) => out.push(CodingRestoreResult {
                path: orig.clone(),
                ok: false,
                error: Some(e),
            }),
        }
    }
    Ok(out)
}

#[cfg(target_os = "macos")]
fn preflight_restore_batch(restore_tokens: &[String], targets: &[PathBuf]) -> Result<(), String> {
    for (restore_token, target) in restore_tokens.iter().zip(targets) {
        let record = trash_restore_records()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(restore_token)
            .cloned()
            .ok_or_else(|| "恢复标识无效或已过期".to_string())?;
        if record.original_path != *target {
            return Err("恢复标识与目标路径不匹配".into());
        }
        let metadata = std::fs::symlink_metadata(&record.trashed_path)
            .map_err(|error| format!("回收站中的条目已不存在：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("拒绝从回收站恢复符号链接".into());
        }
    }
    Ok(())
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn preflight_restore_batch(restore_tokens: &[String], targets: &[PathBuf]) -> Result<(), String> {
    let items =
        trash::os_limited::list().map_err(|error| format!("无法读取系统回收站：{error}"))?;
    for (restore_token, target) in restore_tokens.iter().zip(targets) {
        if Path::new(restore_token) != target {
            return Err("恢复标识与目标路径不匹配".into());
        }
        if !items.iter().any(|item| item.original_path() == *target) {
            return Err(format!("系统回收站中未找到对应条目：{}", target.display()));
        }
    }
    Ok(())
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(unix, not(target_os = "ios"), not(target_os = "android"))
)))]
fn preflight_restore_batch(_restore_tokens: &[String], _targets: &[PathBuf]) -> Result<(), String> {
    Err("当前平台暂不支持从系统回收站恢复".into())
}

/// Resolve a path that does not exist yet while still enforcing that its
/// existing parent is a real directory inside the workspace.
fn resolve_coding_target_path(root: &Path, claimed: &str) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("无法解析工作区：{error}"))?;
    let candidate = PathBuf::from(claimed);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        canonical_root.join(candidate)
    };
    let name = candidate
        .file_name()
        .ok_or_else(|| "恢复目标缺少文件名".to_string())?;
    let parent = candidate
        .parent()
        .ok_or_else(|| "无法解析恢复目标的父目录".to_string())?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|error| format!("无法读取恢复目标的父目录：{error}"))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err("恢复目标的父路径不是工作区内的真实目录".into());
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("无法解析恢复目标的父目录：{error}"))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("拒绝恢复到工作区之外".into());
    }
    Ok(canonical_parent.join(name))
}

#[cfg(target_os = "macos")]
fn restore_trash_item(restore_token: &str, target: &Path) -> Result<(), String> {
    let record = trash_restore_records()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(restore_token)
        .cloned()
        .ok_or_else(|| "恢复标识无效或已过期".to_string())?;
    if record.original_path != target {
        return Err("恢复标识与目标路径不匹配".into());
    }
    let source = record.trashed_path;
    let Some(trash_root) = dirs::home_dir().map(|home| home.join(".Trash")) else {
        return Err("无法定位系统回收站".into());
    };
    let canonical_home_trash = trash_root.canonicalize().ok();
    let metadata = std::fs::symlink_metadata(&source)
        .map_err(|error| format!("回收站中的条目已不存在：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("拒绝从回收站恢复符号链接".into());
    }
    let canonical_source = source
        .canonicalize()
        .map_err(|error| format!("无法解析回收站条目：{error}"))?;
    let effective_uid = unsafe { libc::geteuid() }.to_string();
    let is_volume_trash = canonical_source.ancestors().any(|ancestor| {
        ancestor
            .file_name()
            .is_some_and(|name| name == effective_uid.as_str())
            && ancestor
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == ".Trashes")
    });
    let is_home_trash = canonical_home_trash
        .as_ref()
        .is_some_and(|trash| canonical_source.starts_with(trash));
    if !is_home_trash && !is_volume_trash {
        return Err("恢复标识不属于系统回收站".into());
    }
    std::fs::rename(canonical_source, target)
        .map_err(|error| format!("从系统回收站恢复失败：{error}"))?;
    trash_restore_records()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(restore_token);
    Ok(())
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn restore_trash_item(restore_token: &str, target: &Path) -> Result<(), String> {
    let expected = PathBuf::from(restore_token);
    if expected != target {
        return Err("恢复标识与目标路径不匹配".into());
    }
    let items =
        trash::os_limited::list().map_err(|error| format!("无法读取系统回收站：{error}"))?;
    let item = items
        .into_iter()
        .filter(|item| item.original_path() == expected)
        .max_by_key(|item| item.time_deleted)
        .ok_or_else(|| "系统回收站中未找到对应条目".to_string())?;
    trash::os_limited::restore_all([item]).map_err(|error| format!("从系统回收站恢复失败：{error}"))
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(unix, not(target_os = "ios"), not(target_os = "android"))
)))]
fn restore_trash_item(_restore_token: &str, _target: &Path) -> Result<(), String> {
    Err("当前平台暂不支持从系统回收站恢复".into())
}

// ----- end SP5 -----

#[tauri::command]
pub async fn coding_rename_entry(
    access: State<'_, FilesystemAccess>,
    request: CodingRenameEntryRequest,
) -> Result<CodingRenameResult, String> {
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || rename_entry_blocking(&root, request))
        .await
        .map_err(|error| format!("重命名失败：{error}"))?
}

fn rename_entry_blocking(
    root: &Path,
    request: CodingRenameEntryRequest,
) -> Result<CodingRenameResult, String> {
    let new_name = safe_new_entry_name(&request.new_name)?;
    let old = resolve_coding_path_strict(root, &request.path)?;
    let parent = old.parent().ok_or_else(|| "无法解析父目录".to_string())?;
    let new_path = parent.join(new_name);
    if new_path == old {
        return Ok(CodingRenameResult {
            path: new_path.to_string_lossy().into_owned(),
            old_path: old.to_string_lossy().into_owned(),
        });
    }
    if new_path
        .try_exists()
        .map_err(|error| format!("无法检测目标：{error}"))?
    {
        return Err("同名文件或目录已经存在".into());
    }
    std::fs::rename(&old, &new_path).map_err(|error| format!("重命名失败：{error}"))?;
    Ok(CodingRenameResult {
        path: new_path.to_string_lossy().into_owned(),
        old_path: old.to_string_lossy().into_owned(),
    })
}

#[tauri::command]
pub async fn coding_copy_entries(
    access: State<'_, FilesystemAccess>,
    request: CodingCopyEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    if request.sources.is_empty() {
        return Ok(Vec::new());
    }
    if request.sources.len() > MAX_BATCH_OPS {
        return Err(format!("一次最多复制 {MAX_BATCH_OPS} 个条目"));
    }
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || copy_entries_blocking(&root, request))
        .await
        .map_err(|error| format!("复制失败：{error}"))?
}

fn copy_entries_blocking(
    root: &Path,
    request: CodingCopyEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    let destination = resolve_coding_directory_path(root, Some(&request.destination))?;
    let mut resolved: Vec<PathBuf> = Vec::with_capacity(request.sources.len());
    let mut seen = HashSet::new();
    for raw in &request.sources {
        let src = resolve_coding_path_strict(root, raw)?;
        if !seen.insert(src.clone()) {
            return Err("复制批次包含重复路径".into());
        }
        if src.is_dir() && destination.starts_with(&src) {
            return Err(format!("不能把目录复制到自身内部：{}", src.display()));
        }
        resolved.push(src);
    }
    let mut results = Vec::with_capacity(resolved.len());
    for src in &resolved {
        let display = src.to_string_lossy().into_owned();
        let basename = match src.file_name() {
            Some(name) => name.to_owned(),
            None => {
                results.push(CodingBatchOpResult {
                    path: display.clone(),
                    source_path: display,
                    ok: false,
                    error: Some("源路径缺少文件名".into()),
                    restore_token: None,
                });
                continue;
            }
        };
        let target = destination.join(&basename);
        if target
            .try_exists()
            .map_err(|error| format!("无法检测目标：{error}"))?
        {
            results.push(CodingBatchOpResult {
                path: display.clone(),
                source_path: display,
                ok: false,
                error: Some("目标已存在同名条目".into()),
                restore_token: None,
            });
            continue;
        }
        match copy_one(src, &target) {
            Ok(()) => results.push(CodingBatchOpResult {
                path: target.to_string_lossy().into_owned(),
                source_path: display,
                ok: true,
                error: None,
                restore_token: None,
            }),
            Err(error) => results.push(CodingBatchOpResult {
                path: display.clone(),
                source_path: display,
                ok: false,
                error: Some(format!("{error}")),
                restore_token: None,
            }),
        }
    }
    Ok(results)
}

#[tauri::command]
pub async fn coding_move_entries(
    access: State<'_, FilesystemAccess>,
    request: CodingMoveEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    if request.sources.is_empty() {
        return Ok(Vec::new());
    }
    if request.sources.len() > MAX_BATCH_OPS {
        return Err(format!("一次最多移动 {MAX_BATCH_OPS} 个条目"));
    }
    let root = access.require_workspace(&request.root)?;
    tokio::task::spawn_blocking(move || move_entries_blocking(&root, request))
        .await
        .map_err(|error| format!("移动失败：{error}"))?
}

fn move_entries_blocking(
    root: &Path,
    request: CodingMoveEntryRequest,
) -> Result<Vec<CodingBatchOpResult>, String> {
    let destination = resolve_coding_directory_path(root, Some(&request.destination))?;
    let mut resolved: Vec<PathBuf> = Vec::with_capacity(request.sources.len());
    let mut seen = HashSet::new();
    for raw in &request.sources {
        let src = resolve_coding_path_strict(root, raw)?;
        if !seen.insert(src.clone()) {
            return Err("移动批次包含重复路径".into());
        }
        if src.is_dir() && destination.starts_with(&src) {
            return Err(format!("不能把目录移动到自身内部：{}", src.display()));
        }
        resolved.push(src);
    }
    let mut results = Vec::with_capacity(resolved.len());
    for src in &resolved {
        let display = src.to_string_lossy().into_owned();
        let basename = match src.file_name() {
            Some(name) => name.to_owned(),
            None => {
                results.push(CodingBatchOpResult {
                    path: display.clone(),
                    source_path: display,
                    ok: false,
                    error: Some("源路径缺少文件名".into()),
                    restore_token: None,
                });
                continue;
            }
        };
        let target = destination.join(&basename);
        if target == *src {
            results.push(CodingBatchOpResult {
                path: target.to_string_lossy().into_owned(),
                source_path: display,
                ok: true,
                error: None,
                restore_token: None,
            });
            continue;
        }
        if target
            .try_exists()
            .map_err(|error| format!("无法检测目标：{error}"))?
        {
            results.push(CodingBatchOpResult {
                path: display.clone(),
                source_path: display,
                ok: false,
                error: Some("目标已存在同名条目".into()),
                restore_token: None,
            });
            continue;
        }
        match std::fs::rename(src, &target) {
            Ok(()) => results.push(CodingBatchOpResult {
                path: target.to_string_lossy().into_owned(),
                source_path: display,
                ok: true,
                error: None,
                restore_token: None,
            }),
            Err(error) => {
                // Cross-volume fallback: copy, remove the source, and roll the
                // fresh copy back if source removal fails.
                let is_cross_volume = error.raw_os_error() == Some(libc::EXDEV);
                if !is_cross_volume {
                    results.push(CodingBatchOpResult {
                        path: display.clone(),
                        source_path: display,
                        ok: false,
                        error: Some(format!("移动失败：{error}")),
                        restore_token: None,
                    });
                    continue;
                }
                let copy_result = copy_one(src, &target);
                if let Err(copy_err) = copy_result {
                    results.push(CodingBatchOpResult {
                        path: display.clone(),
                        source_path: display,
                        ok: false,
                        error: Some(format!("跨卷复制失败：{copy_err}")),
                        restore_token: None,
                    });
                    continue;
                }
                match fallback_remove(src) {
                    Ok(()) => results.push(CodingBatchOpResult {
                        path: target.to_string_lossy().into_owned(),
                        source_path: display,
                        ok: true,
                        error: None,
                        restore_token: None,
                    }),
                    Err(remove_error) => {
                        let rollback = fallback_remove(&target);
                        results.push(CodingBatchOpResult {
                            path: display.clone(),
                            source_path: display,
                            ok: false,
                            error: Some(match rollback {
                                Ok(()) => format!("跨卷移动未完成：{remove_error}"),
                                Err(rollback_error) => format!(
                                    "跨卷移动未完成，且临时副本清理失败：{remove_error}；{rollback_error}"
                                ),
                            }),
                            restore_token: None,
                        });
                    }
                }
            }
        }
    }
    Ok(results)
}

#[tauri::command]
pub async fn coding_terminal_create(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    processes: State<'_, CodingProcesses>,
    root: String,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<CodingTerminalCreated, String> {
    let root = access.require_workspace(&root)?;
    #[cfg(target_os = "windows")]
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    #[cfg(not(target_os = "windows"))]
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| Path::new(value).is_absolute() && Path::new(value).is_file())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let config = PtyConfig {
        command: vec![shell],
        cols: cols.unwrap_or(100).clamp(20, 500),
        rows: rows.unwrap_or(28).clamp(5, 200),
        cwd: Some(root),
        env: HashMap::new(),
    };
    let handle = tokio::task::spawn_blocking(move || PtyHandle::spawn(&config))
        .await
        .map_err(|error| format!("终端启动任务失败：{error}"))?
        .map_err(|error| format!("无法创建交互式终端：{error}"))?;
    let (master, child, mut reader, writer) = handle.into_parts();
    let terminal_id = uuid::Uuid::now_v7().to_string();
    let session = Arc::new(CodingTerminalSession {
        master,
        child: Mutex::new(child),
        writer: Mutex::new(writer),
    });
    {
        let mut terminals = processes
            .terminals
            .lock()
            .map_err(|_| "终端状态已损坏".to_string())?;
        terminals.retain(|_, session| session.child.lock().is_ok_and(|mut child| child.is_alive()));
        if terminals.len() >= 8 {
            if let Ok(mut child) = session.child.lock() {
                let _ = child.kill();
            }
            return Err("代码工作台最多同时保留 8 个终端".into());
        }
        terminals.insert(terminal_id.clone(), session);
    }
    let event_terminal_id = terminal_id.clone();
    let output_thread = std::thread::Builder::new()
        .name(format!("coding-pty-{}", &terminal_id[..8]))
        .spawn(move || {
            let mut buffer = [0u8; 8 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let _ = app.emit(
                            "coding://terminal-output",
                            CodingTerminalEvent {
                                terminal_id: event_terminal_id.clone(),
                                data_base64: base64::engine::general_purpose::STANDARD
                                    .encode(&buffer[..count]),
                            },
                        );
                    }
                }
            }
            let _ = app.emit(
                "coding://terminal-exit",
                CodingTerminalEvent {
                    terminal_id: event_terminal_id,
                    data_base64: String::new(),
                },
            );
        });
    if let Err(error) = output_thread {
        if let Ok(mut terminals) = processes.terminals.lock() {
            if let Some(session) = terminals.remove(&terminal_id) {
                if let Ok(mut child) = session.child.lock() {
                    let _ = terminate_terminal_child(&mut child);
                }
            }
        }
        return Err(format!("无法启动终端输出线程：{error}"));
    }
    Ok(CodingTerminalCreated { terminal_id })
}

#[tauri::command]
pub async fn coding_terminal_write(
    processes: State<'_, CodingProcesses>,
    terminal_id: String,
    data: String,
) -> Result<(), String> {
    if data.len() > 64 * 1024 || data.contains('\0') {
        return Err("终端输入超过 64 KB 或包含空字符".into());
    }
    let session = processes
        .terminals
        .lock()
        .map_err(|_| "终端状态已损坏".to_string())?
        .get(&terminal_id)
        .cloned()
        .ok_or_else(|| "终端不存在或已经关闭".to_string())?;
    tokio::task::spawn_blocking(move || {
        let mut writer = session
            .writer
            .lock()
            .map_err(|_| "终端输入状态已损坏".to_string())?;
        writer
            .write_all(data.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| format!("写入终端失败：{error}"))
    })
    .await
    .map_err(|error| format!("终端输入任务失败：{error}"))?
}

#[tauri::command]
pub async fn coding_terminal_resize(
    processes: State<'_, CodingProcesses>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let session = processes
        .terminals
        .lock()
        .map_err(|_| "终端状态已损坏".to_string())?
        .get(&terminal_id)
        .cloned()
        .ok_or_else(|| "终端不存在或已经关闭".to_string())?;
    session
        .master
        .resize(cols.clamp(20, 500), rows.clamp(5, 200))
        .map_err(|error| format!("调整终端尺寸失败：{error}"))
}

#[tauri::command]
pub async fn coding_terminal_close(
    processes: State<'_, CodingProcesses>,
    terminal_id: String,
) -> Result<bool, String> {
    let session = processes
        .terminals
        .lock()
        .map_err(|_| "终端状态已损坏".to_string())?
        .remove(&terminal_id);
    let Some(session) = session else {
        return Ok(false);
    };
    tokio::task::spawn_blocking(move || {
        let mut child = session
            .child
            .lock()
            .map_err(|_| "终端进程状态已损坏".to_string())?;
        terminate_terminal_child(&mut child)
    })
    .await
    .map_err(|error| format!("关闭终端任务失败：{error}"))??;
    Ok(true)
}

fn fallback_code_search(
    root: &Path,
    query: &str,
    options: &CodingSearchOptions,
) -> Vec<CodingSearchHit> {
    let case_sensitive = options.case_sensitive;
    let pattern = if options.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let pattern = if options.whole_word {
        format!(r"\b(?:{pattern})\b")
    } else {
        pattern
    };
    let matcher = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
    {
        Ok(matcher) => matcher,
        Err(_) => return Vec::new(),
    };
    let mut hits = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut scanned = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if scanned >= MAX_SCANNED_FILES || hits.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= MAX_SCANNED_FILES || hits.len() >= MAX_SEARCH_RESULTS {
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if depth < MAX_SCAN_DEPTH
                    && !IGNORED_DIRECTORIES.contains(&name.as_str())
                    && !(name.starts_with('.')
                        && !matches!(name.as_str(), ".github" | ".echoagent"))
                {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = relative_display(root, &path);
            if !search_path_allowed(&relative, options) {
                continue;
            }
            scanned += 1;
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > 1024 * 1024 {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (line_index, line) in content.lines().enumerate() {
                if let Some(found) = matcher.find(line) {
                    hits.push(CodingSearchHit {
                        path: relative.clone(),
                        line: line_index + 1,
                        column: line[..found.start()].chars().count() + 1,
                        preview: bounded_search_preview(line),
                    });
                    if hits.len() >= MAX_SEARCH_RESULTS {
                        break;
                    }
                }
            }
        }
    }
    hits.sort_by(|left, right| left.path.cmp(&right.path).then(left.line.cmp(&right.line)));
    hits
}

fn search_glob_matches(path: &str, raw: &str) -> bool {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|glob| {
            // Match basename-only patterns at any depth, like ripgrep's --glob.
            let candidate = if glob.contains('/') {
                path
            } else {
                path.rsplit('/').next().unwrap_or(path)
            };
            let mut pattern = String::from("^");
            let mut chars = glob.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    '*' if chars.peek() == Some(&'*') => {
                        chars.next();
                        if chars.peek() == Some(&'/') {
                            chars.next();
                            pattern.push_str("(?:.*/)?");
                        } else {
                            pattern.push_str(".*");
                        }
                    }
                    '*' => pattern.push_str("[^/]*"),
                    '?' => pattern.push_str("[^/]"),
                    _ => pattern.push_str(&regex::escape(&ch.to_string())),
                }
            }
            pattern.push('$');
            regex::Regex::new(&pattern).is_ok_and(|matcher| matcher.is_match(candidate))
        })
}

fn search_path_allowed(path: &str, options: &CodingSearchOptions) -> bool {
    (options.include_glob.trim().is_empty() || search_glob_matches(path, &options.include_glob))
        && (options.exclude_glob.trim().is_empty()
            || !search_glob_matches(path, &options.exclude_glob))
}

fn bounded_search_preview(line: &str) -> String {
    let trimmed = line.trim_end();
    let mut preview = trimmed
        .chars()
        .take(MAX_SEARCH_PREVIEW_CHARS)
        .collect::<String>();
    if trimmed.chars().count() > MAX_SEARCH_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

#[tauri::command]
pub async fn coding_search_workspace(
    access: State<'_, FilesystemAccess>,
    root: String,
    query: String,
    options: Option<CodingSearchOptions>,
) -> Result<Vec<CodingSearchHit>, String> {
    let root = access.require_workspace(&root)?;
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 256 || query.contains('\0') {
        return Err("搜索词不能为空且不能超过 256 个字符".into());
    }
    let options = options.unwrap_or_default();
    for value in [&options.include_glob, &options.exclude_glob] {
        if value.chars().count() > 512 || value.split(',').count() > 16 || value.contains('\0') {
            return Err("文件包含/排除规则过长或数量过多".into());
        }
        if value.split(',').map(str::trim).any(|glob| {
            glob.starts_with('!') || glob.chars().any(|ch| matches!(ch, '[' | ']' | '{' | '}'))
        }) {
            return Err("文件规则仅支持逗号分隔的 *、** 和 ? 通配符".into());
        }
    }
    if options.regex && regex::Regex::new(query).is_err() {
        return Err("正则表达式无效".into());
    }
    let mut command = crate::process_supervisor::background_async_command("rg");
    command.args([
        "--line-number",
        "--column",
        "--no-heading",
        "--no-messages",
        "--color",
        "never",
        "--max-count",
        "100",
        "--max-filesize",
        "4M",
        "--glob",
        "!.git/**",
        "--glob",
        "!node_modules/**",
        "--glob",
        "!target/**",
        "--glob",
        "!dist/**",
        "--glob",
        "!build/**",
    ]);
    if options.case_sensitive {
        command.arg("--case-sensitive");
    } else {
        command.arg("--ignore-case");
    }
    if !options.regex {
        command.arg("--fixed-strings");
    }
    if options.whole_word {
        command.arg("--word-regexp");
    }
    for glob in options
        .include_glob
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.args(["--glob", glob]);
    }
    for glob in options
        .exclude_glob
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.args(["--glob", &format!("!{glob}")]);
    }
    command
        .args(["--", query, "."])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No ripgrep on this machine: fall back to a synchronous walk, which
            // must run on the blocking pool rather than a tokio worker.
            let root = root.clone();
            let owned_query = query.to_string();
            return tokio::task::spawn_blocking(move || {
                fallback_code_search(&root, &owned_query, &options)
            })
            .await
            .map_err(|error| format!("代码搜索失败：{error}"));
        }
        Err(error) => return Err(format!("无法启动代码搜索：{error}")),
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取代码搜索结果".to_string())?;
    let mut lines = BufReader::new(stdout).lines();
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取代码搜索错误".to_string())?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let mut hits = Vec::new();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("读取代码搜索结果失败：{error}"))?
    {
        let mut columns = line.splitn(4, ':');
        let Some(path) = columns.next() else { continue };
        let Some(line_number) = columns.next().and_then(|value| value.parse::<usize>().ok()) else {
            continue;
        };
        let Some(byte_column) = columns.next().and_then(|value| value.parse::<usize>().ok()) else {
            continue;
        };
        let raw_preview = columns.next().unwrap_or_default();
        let column = raw_preview
            .char_indices()
            .take_while(|(offset, _)| *offset < byte_column.saturating_sub(1))
            .count()
            + 1;
        let preview = bounded_search_preview(raw_preview);
        hits.push(CodingSearchHit {
            path: path.trim_start_matches("./").replace('\\', "/"),
            line: line_number,
            column,
            preview,
        });
        if hits.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }
    let capped = hits.len() >= MAX_SEARCH_RESULTS;
    let status = if capped {
        let _ = child.kill().await;
        child.wait().await.ok()
    } else {
        Some(
            child
                .wait()
                .await
                .map_err(|error| format!("等待代码搜索完成失败：{error}"))?,
        )
    };
    let stderr = stderr_task
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    if !capped && !status.is_some_and(|status| status.success() || status.code() == Some(1)) {
        return Err(format!(
            "代码搜索失败：{}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(hits)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AnalysisProgress {
    root: String,
    scanned: usize,
}

/// Result of the synchronous half of a workspace scan.
struct WorkspaceScan {
    file_count: usize,
    truncated: bool,
    language_counts: BTreeMap<String, usize>,
    manifests: Vec<ManifestCandidate>,
    instruction_files: Vec<String>,
    modules: Vec<CodingModule>,
}

/// Walk the repository and derive languages, manifests, rule files and modules.
///
/// This touches up to `MAX_SCANNED_FILES` entries with synchronous IO, so it
/// must never run on a tokio worker thread — `coding_analyze_workspace` hands it
/// to the blocking pool. Progress is emitted as it goes so a large repository
/// shows movement instead of appearing frozen.
fn scan_workspace_blocking(app: &AppHandle, root: &Path) -> WorkspaceScan {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut file_count = 0usize;
    let mut truncated = false;
    let mut language_counts = BTreeMap::<String, usize>::new();
    let mut manifests = Vec::<ManifestCandidate>::new();
    let mut instruction_files = Vec::<String>::new();
    let mut next_progress_at = PROGRESS_EVERY_FILES;

    while let Some((directory, depth)) = stack.pop() {
        if file_count >= MAX_SCANNED_FILES {
            truncated = true;
            break;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if file_count >= MAX_SCANNED_FILES {
                truncated = true;
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if depth < MAX_SCAN_DEPTH
                    && !IGNORED_DIRECTORIES.contains(&name.as_str())
                    && !(name.starts_with('.')
                        && !matches!(name.as_str(), ".github" | ".echoagent"))
                {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            file_count += 1;
            if file_count >= next_progress_at {
                next_progress_at += PROGRESS_EVERY_FILES;
                let _ = app.emit(
                    "coding://analysis-progress",
                    AnalysisProgress {
                        root: root.to_string_lossy().into_owned(),
                        scanned: file_count,
                    },
                );
            }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "agents.md" | "claude.md" | "copilot-instructions.md" | ".cursorrules"
            ) || relative_display(root, &path).starts_with(".echoagent/rules/")
            {
                instruction_files.push(relative_display(root, &path));
            }
            if let Some(kind) = manifest_kind(&name) {
                manifests.push(ManifestCandidate {
                    path: path.clone(),
                    kind,
                });
            }
            if let Some(language) = path
                .extension()
                .and_then(|value| value.to_str())
                .and_then(language_for_extension)
            {
                *language_counts.entry(language.to_string()).or_default() += 1;
            }
        }
    }
    manifests.sort_by(|left, right| left.path.cmp(&right.path));

    let mut seen_module_roots = HashSet::new();
    let mut modules = Vec::<CodingModule>::new();
    for manifest in &manifests {
        let Some(module_root) = manifest.path.parent() else {
            continue;
        };
        let key = module_root.to_path_buf();
        if !seen_module_roots.insert(key) {
            continue;
        }
        modules.push(CodingModule {
            name: module_name(manifest, module_root),
            path: relative_display(root, module_root),
            kind: manifest.kind.to_string(),
            dependencies: Vec::new(),
        });
    }
    if modules.is_empty() {
        modules.push(CodingModule {
            name: root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("workspace")
                .to_string(),
            path: ".".into(),
            kind: "Generic".into(),
            dependencies: Vec::new(),
        });
    }

    // A bounded, deterministic local dependency graph. Manifest references to
    // another discovered module are enough to produce a trustworthy edge;
    // ambiguous source-level guesses are deliberately omitted.
    let module_names = modules
        .iter()
        .map(|module| module.name.clone())
        .collect::<Vec<_>>();
    for module in &mut modules {
        let module_root = root.join(&module.path);
        let manifest_text = manifests
            .iter()
            .filter(|candidate| candidate.path.parent() == Some(module_root.as_path()))
            .filter_map(|candidate| bounded_text(&candidate.path))
            .collect::<Vec<_>>()
            .join("\n");
        module.dependencies = module_names
            .iter()
            .filter(|candidate| {
                candidate.as_str() != module.name.as_str()
                    && manifest_text.contains(candidate.as_str())
            })
            .cloned()
            .collect();
        module.dependencies.sort();
        module.dependencies.dedup();
    }
    modules.sort_by(|left, right| left.path.cmp(&right.path));

    WorkspaceScan {
        file_count,
        truncated,
        language_counts,
        manifests,
        instruction_files,
        modules,
    }
}

#[tauri::command]
pub async fn coding_analyze_workspace(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<CodingWorkspaceAnalysis, String> {
    let root = access.require_workspace(&root)?;
    // The directory walk is synchronous and unbounded in time; running it on a
    // worker thread would freeze every other Tauri command until it finished.
    let scan = {
        let app = app.clone();
        let root = root.clone();
        tokio::task::spawn_blocking(move || scan_workspace_blocking(&app, &root))
            .await
            .map_err(|error| format!("工程分析失败：{error}"))?
    };
    let WorkspaceScan {
        file_count,
        truncated,
        language_counts,
        manifests,
        mut instruction_files,
        modules,
    } = scan;

    let mut languages = language_counts
        .into_iter()
        .map(|(language, files)| CodingLanguageStat { language, files })
        .collect::<Vec<_>>();
    languages.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| left.language.cmp(&right.language))
    });

    let git_top = git_output(&root, &["rev-parse", "--show-toplevel"]).await;
    let has_git = git_top.is_some();
    let git_branch = if has_git {
        git_output(&root, &["branch", "--show-current"])
            .await
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    let git_changed_files = if has_git {
        git_output(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )
        .await
        .map(|value| value.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or_default()
    } else {
        0
    };
    let mut project_kinds = manifests
        .iter()
        .map(|manifest| manifest.kind)
        .collect::<Vec<_>>();
    project_kinds.sort();
    project_kinds.dedup();
    let project_type = project_kinds.join(" + ");
    let commands = validation_commands(&root, &manifests);
    instruction_files.sort();
    instruction_files.dedup();
    instruction_files.truncate(64);

    Ok(CodingWorkspaceAnalysis {
        root: root.to_string_lossy().into_owned(),
        name: root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_string(),
        project_type,
        file_count,
        truncated,
        languages,
        modules,
        validation_commands: commands,
        has_git,
        git_branch,
        git_changed_files,
        instruction_files,
        scanned_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Public so the verification engine reuses the same escape-sequence cleanup
/// instead of storing raw terminal control codes in its records.
pub fn strip_ansi(value: String) -> String {
    let expression = regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("valid ANSI regex");
    expression.replace_all(&value, "").into_owned()
}

fn dangerous_delete_target(raw: &str, workspace_root: &Path) -> bool {
    let unquoted = raw
        .trim_matches(|character| character == '\'' || character == '"')
        .trim_end_matches(['/', '\\']);
    let value = unquoted.to_ascii_lowercase();
    value.is_empty()
        || matches!(
            value.as_str(),
            "/" | "."
                | "./"
                | ".."
                | "~"
                | "$home"
                | "${home}"
                | "$pwd"
                | "${pwd}"
                | "$(pwd)"
                | "`pwd`"
                | "%cd%"
        )
        || value.contains('*')
        || value == ".git"
        || value.starts_with(".git/")
        || value.ends_with("/.git")
        || value.contains("/.git/")
        || matches!(
            value.as_str(),
            "/usr"
                | "/var"
                | "/etc"
                | "/bin"
                | "/sbin"
                | "/lib"
                | "/opt"
                | "/boot"
                | "/root"
                | "/home"
                | "/system"
                | "/library"
                | "/tmp"
                | "/dev"
        )
        || {
            let canonical_workspace = workspace_root
                .canonicalize()
                .unwrap_or_else(|_| workspace_root.to_path_buf());
            let candidate = Path::new(unquoted);
            let candidate = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                canonical_workspace.join(candidate)
            };
            candidate
                .canonicalize()
                .ok()
                .is_some_and(|target| canonical_workspace.starts_with(target))
        }
}

/// Defense in depth for the renderer-facing command endpoint. The UI already
/// presents risk confirmation, but a forged IPC call must still be unable to
/// execute commands that discard repositories or damage system paths.
/// Public so the verification engine reuses this single high-risk policy rather
/// than maintaining a second copy of the rules.
pub fn high_risk_command_reason(command: &str, workspace_root: &Path) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    let patterns = [
        (r"\bdd\s+[^\r\n]*\bof=/dev/", "禁止 dd 写入块设备"),
        (r"(?:^|[;&|]\s*)mkfs(?:\.|\s)", "禁止格式化文件系统"),
        (r"\bchmod\s+777\s+/", "禁止修改系统根目录权限"),
        (r"\bgit\s+reset\s+--hard\b", "禁止丢弃 Git 工作区改动"),
        (
            r"\bgit\s+clean\s+[^\r\n]*-[fdx]+",
            "禁止强制清理 Git 未跟踪文件",
        ),
        (r"\bgit\s+push\s+[^\r\n]*--force\b", "禁止强制覆盖远端分支"),
        (
            r"\bgit\s+checkout\s+(?:-f|--force)\b",
            "禁止强制签出并丢弃改动",
        ),
        (r"\bgit\s+checkout\s+--\s+\.", "禁止批量还原全部改动"),
        (
            r"\bfind\s+[^\r\n]*(?:-exec|-delete)\b",
            "禁止 find 批量执行或删除",
        ),
    ];
    for (pattern, reason) in patterns {
        if regex::Regex::new(pattern)
            .expect("valid command policy regex")
            .is_match(&normalized)
        {
            return Some(reason);
        }
    }

    for segment in normalized.split([';', '|', '&']) {
        let tokens = segment.split_whitespace().collect::<Vec<_>>();
        let Some(rm_index) = tokens
            .iter()
            .position(|token| matches!(*token, "rm" | "\\rm") || token.ends_with("/rm"))
        else {
            continue;
        };
        let arguments = &tokens[rm_index + 1..];
        let recursive = arguments.iter().any(|token| {
            matches!(*token, "--recursive")
                || (token.starts_with('-')
                    && token[1..].chars().any(|flag| matches!(flag, 'r' | 'R')))
        });
        let force = arguments.iter().any(|token| {
            matches!(*token, "--force") || (token.starts_with('-') && token[1..].contains('f'))
        });
        if recursive
            && force
            && arguments
                .iter()
                .filter(|token| !token.starts_with('-'))
                .any(|token| dangerous_delete_target(token, workspace_root))
        {
            return Some("禁止递归强制删除危险路径");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_approval_is_bound_and_one_shot() {
        let workspace = tempfile::tempdir().unwrap();
        let processes = CodingProcesses::default();
        let token = processes
            .approve_verification(workspace.path(), "task-1", "pnpm test -- auth")
            .unwrap();
        assert!(processes
            .consume_verification_approval(
                &token,
                workspace.path(),
                "another-task",
                "pnpm test -- auth",
            )
            .is_err());
        // A mismatched attempt consumes the grant, so it cannot be replayed.
        assert!(processes
            .consume_verification_approval(&token, workspace.path(), "task-1", "pnpm test -- auth",)
            .is_err());

        let token = processes
            .approve_verification(workspace.path(), "task-1", "pnpm test -- auth")
            .unwrap();
        processes
            .consume_verification_approval(&token, workspace.path(), "task-1", "pnpm test -- auth")
            .unwrap();
        assert!(processes
            .consume_verification_approval(&token, workspace.path(), "task-1", "pnpm test -- auth",)
            .is_err());
    }

    #[test]
    fn recognizes_common_languages_and_manifests() {
        assert_eq!(language_for_extension("tsx"), Some("TypeScript"));
        assert_eq!(language_for_extension("java"), Some("Java"));
        assert_eq!(editor_language(Path::new("src/view.tsx")), "typescript");
        assert_eq!(editor_language(Path::new("src/view.jsx")), "javascript");
        assert_eq!(manifest_kind("pom.xml"), Some("Maven"));
        assert_eq!(manifest_kind("package.json"), Some("Node.js"));
    }

    #[test]
    fn extracts_minimal_manifest_names() {
        assert_eq!(
            first_tag(
                "<project><artifactId>orders</artifactId></project>",
                "artifactId"
            )
            .as_deref(),
            Some("orders")
        );
        assert_eq!(
            quoted_toml_value("name = \"workspace\"", "name").as_deref(),
            Some("workspace")
        );
    }

    #[test]
    fn validation_commands_cover_polyglot_roots() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("pom.xml"), "<project />").unwrap();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest","build":"vite build"}}"#,
        )
        .unwrap();
        let manifests = vec![
            ManifestCandidate {
                path: temp.path().join("pom.xml"),
                kind: "Maven",
            },
            ManifestCandidate {
                path: temp.path().join("package.json"),
                kind: "Node.js",
            },
        ];
        let commands = validation_commands(temp.path(), &manifests);
        assert!(commands.contains(&"mvn test".to_string()));
        assert!(commands.contains(&"npm test".to_string()));
        assert!(commands.contains(&"npm run build".to_string()));
    }

    #[test]
    fn validation_commands_respect_nested_modules_and_declared_node_scripts() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("web");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .unwrap();
        let manifests = vec![ManifestCandidate {
            path: package.join("package.json"),
            kind: "Node.js",
        }];
        let commands = validation_commands(temp.path(), &manifests);
        assert_eq!(commands, vec!["npm --prefix \"web\" test"]);
    }

    #[test]
    fn native_policy_rejects_destructive_commands_even_inside_shell_chains() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let parent = root.parent().unwrap().to_string_lossy();
        assert!(high_risk_command_reason("rm -rf /", root).is_some());
        assert!(high_risk_command_reason("rm -rf .", root).is_some());
        assert!(high_risk_command_reason("rm -rf ./", root).is_some());
        assert!(high_risk_command_reason("rm -rf \"$PWD\"", root).is_some());
        assert!(high_risk_command_reason(&format!("rm -rf \"{parent}\""), root).is_some());
        assert!(high_risk_command_reason("cd src && rm -rf ..", root).is_some());
        assert!(high_risk_command_reason("echo done && rm -rf .git", root).is_some());
        assert!(high_risk_command_reason("git reset --hard HEAD~1", root).is_some());
        assert!(high_risk_command_reason("find . -name '*.tmp' -delete", root).is_some());
        assert!(high_risk_command_reason("rm -rf build", root).is_none());
        assert!(high_risk_command_reason("mvn test", root).is_none());
    }

    #[test]
    fn parses_nul_delimited_git_status_without_guessing_from_chat_messages() {
        let stats = HashMap::from([
            ("src/app.ts".to_string(), (4usize, 1usize)),
            ("src/new.ts".to_string(), (7usize, 0usize)),
        ]);
        let files = parse_git_status(
            b" M src/app.ts\0?? src/new.ts\0R  src/renamed.ts\0src/old.ts\0",
            &stats,
        );
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/app.ts");
        assert_eq!((files[0].added, files[0].removed), (4, 1));
        assert_eq!(files[1].status, "untracked");
        assert_eq!(files[2].old_path.as_deref(), Some("src/old.ts"));
    }

    #[test]
    fn git_diff_paths_allow_deleted_files_but_reject_workspace_escape() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_git_relative_path(temp.path(), "src/deleted.ts").unwrap(),
            "src/deleted.ts"
        );
        assert!(normalize_git_relative_path(temp.path(), "../secret").is_err());
        assert!(normalize_git_relative_path(temp.path(), "/tmp/outside").is_err());
    }

    #[test]
    fn new_entries_are_single_safe_names() {
        assert_eq!(safe_new_entry_name("feature.ts").unwrap(), "feature.ts");
        assert!(safe_new_entry_name("../outside").is_err());
        assert!(safe_new_entry_name("nested/file.ts").is_err());
        assert!(safe_new_entry_name("nested\\file.ts").is_err());
        assert!(safe_new_entry_name(".git").is_err());
        assert!(safe_new_entry_name("").is_err());
    }

    #[test]
    fn new_entries_stay_inside_real_workspace_directories() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("src");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(
            resolve_coding_directory_path(workspace.path(), Some("src")).unwrap(),
            nested.canonicalize().unwrap()
        );
        assert!(resolve_coding_directory_path(workspace.path(), outside.path().to_str()).is_err());
        assert!(resolve_coding_directory_path(workspace.path(), Some("missing")).is_err());
    }

    #[tokio::test]
    async fn git_stage_helper_uses_one_safe_workspace_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let init = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q"])
            .output()
            .await
            .unwrap();
        assert!(init.status.success());
        std::fs::write(root.join("safe.txt"), "safe\n").unwrap();
        let path = normalize_git_relative_path(root, "safe.txt").unwrap();
        let staged = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["add", "--", path.as_str()])
            .output()
            .await
            .unwrap();
        assert!(staged.status.success());
        assert!(normalize_git_relative_path(root, "../../outside").is_err());
    }

    #[test]
    fn fallback_search_is_bounded_and_returns_real_locations() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("src/app.ts"),
            "export const cancelOrder = true;\n",
        )
        .unwrap();
        let hits =
            fallback_code_search(temp.path(), "cancelOrder", &CodingSearchOptions::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/app.ts");
        assert_eq!((hits[0].line, hits[0].column), (1, 14));
    }

    #[test]
    fn fallback_search_honors_match_and_file_filters() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/app.ts"), "Foo food foo\n").unwrap();
        std::fs::write(temp.path().join("src/app.test.ts"), "foo\n").unwrap();
        std::fs::write(temp.path().join("README.md"), "foo\n").unwrap();
        let options = CodingSearchOptions {
            case_sensitive: false,
            whole_word: true,
            regex: false,
            include_glob: "*.ts".into(),
            exclude_glob: "**/*.test.ts".into(),
        };
        let hits = fallback_code_search(temp.path(), "foo", &options);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/app.ts");
        assert_eq!(hits[0].column, 1);
    }

    #[cfg(unix)]
    #[test]
    fn interactive_pty_starts_inside_the_selected_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let config = PtyConfig {
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "printf '%s' \"$PWD\"".to_string(),
            ],
            cols: 80,
            rows: 24,
            cwd: Some(temp.path().to_path_buf()),
            env: HashMap::new(),
        };
        let handle = PtyHandle::spawn(&config).expect("PTY should start");
        let (master, mut child, mut reader, writer) = handle.into_parts();
        let read_thread = std::thread::spawn(move || {
            let mut bytes = vec![0u8; 4 * 1024];
            let count = reader
                .read(&mut bytes)
                .expect("PTY output should be readable");
            bytes.truncate(count);
            String::from_utf8_lossy(&bytes).into_owned()
        });
        assert_eq!(child.wait().expect("PTY child should finish"), 0);
        drop(writer);
        drop(master);
        let output = read_thread.join().expect("PTY reader should finish");
        assert!(output.contains(temp.path().to_string_lossy().as_ref()));
    }

    // ----- SP1: file ops (delete / rename / copy / move) -----

    fn make_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), "bravo").unwrap();
        dir
    }

    fn workspace_str(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    #[test]
    fn rename_entry_rejects_existing_target_and_applies_new_name() {
        let dir = make_workspace();
        let root = dir.path();

        let req = CodingRenameEntryRequest {
            root: workspace_str(&dir),
            path: root.join("a.txt").to_string_lossy().into_owned(),
            new_name: "renamed.txt".into(),
        };
        let result = rename_entry_blocking(root, req).expect("rename should succeed");
        assert!(result.path.ends_with("renamed.txt"));
        assert!(result.old_path.ends_with("a.txt"));
        assert!(root.join("renamed.txt").exists());

        // First conflict variant: rename to "b.txt" — b.txt lives under sub/,
        // not under root, so this should actually succeed (no collision).
        let rename_into_subdir = CodingRenameEntryRequest {
            root: workspace_str(&dir),
            path: root.join("renamed.txt").to_string_lossy().into_owned(),
            new_name: "b.txt".into(),
        };
        rename_entry_blocking(root, rename_into_subdir)
            .expect("rename to non-colliding name should succeed");

        // create a colliding target in the same parent directory
        std::fs::write(root.join("renamed.txt"), "x").unwrap();
        let conflict = CodingRenameEntryRequest {
            root: workspace_str(&dir),
            path: root.join("renamed.txt").to_string_lossy().into_owned(),
            new_name: "collide.txt".into(),
        };
        std::fs::write(root.join("collide.txt"), "x").unwrap();
        let err = rename_entry_blocking(root, conflict).unwrap_err();
        assert!(
            err.contains("已经存在"),
            "expected conflict error, got {err}"
        );
    }

    #[test]
    fn rename_entry_rejects_invalid_names() {
        let dir = make_workspace();
        let root = dir.path();
        let req = CodingRenameEntryRequest {
            root: workspace_str(&dir),
            path: root.join("a.txt").to_string_lossy().into_owned(),
            new_name: "../escape.txt".into(),
        };
        assert!(rename_entry_blocking(root, req).is_err());
    }

    #[test]
    fn delete_entries_moves_to_trash_or_keeps_source_and_reports_per_item() {
        let dir = make_workspace();
        let root = dir.path();
        let a = root.join("a.txt").to_string_lossy().into_owned();
        let sub = root.join("sub").to_string_lossy().into_owned();

        let req = CodingDeleteEntryRequest {
            root: workspace_str(&dir),
            paths: vec![a.clone(), sub.clone()],
        };
        let results = delete_entries_blocking(root, req).expect("delete should run");
        assert_eq!(results.len(), 2);
        // Headless CI may not expose a system trash. A failure must keep the
        // source intact; a success must provide a restore token and remove it.
        for r in &results {
            assert_eq!(
                r.ok,
                r.restore_token.is_some(),
                "unexpected result: {:?}",
                r
            );
            #[cfg(target_os = "macos")]
            if let Some(token) = &r.restore_token {
                assert!(uuid::Uuid::parse_str(token).is_ok(), "token must be opaque");
            }
            let source_exists = Path::new(&r.source_path).exists();
            assert_eq!(source_exists, !r.ok, "source/result mismatch: {:?}", r);
        }

        // Restore every successfully trashed item so the test exercises file
        // and directory round-trips and does not leave artifacts in Trash.
        let successful = results
            .iter()
            .filter(|result| result.ok)
            .collect::<Vec<_>>();
        if !successful.is_empty() {
            let restored = restore_blocking(
                root,
                CodingRestoreRequest {
                    root: workspace_str(&dir),
                    original_paths: successful
                        .iter()
                        .map(|result| result.source_path.clone())
                        .collect(),
                    restore_tokens: successful
                        .iter()
                        .map(|result| result.restore_token.clone().unwrap())
                        .collect(),
                },
            )
            .expect("restore should run");
            assert!(restored.iter().all(|result| result.ok), "{restored:?}");
            for result in successful {
                assert!(Path::new(&result.source_path).exists());
            }
        }
    }

    #[test]
    fn restore_batch_preflight_prevents_partial_restore_on_collision() {
        let dir = make_workspace();
        let root = dir.path();
        let missing = root.join("missing.txt");
        let existing = root.join("a.txt");
        let results = restore_blocking(
            root,
            CodingRestoreRequest {
                root: workspace_str(&dir),
                original_paths: vec![
                    missing.to_string_lossy().into_owned(),
                    existing.to_string_lossy().into_owned(),
                ],
                restore_tokens: vec!["unused-1".into(), "unused-2".into()],
            },
        )
        .unwrap();
        assert!(results.iter().all(|result| !result.ok));
        assert!(
            !missing.exists(),
            "valid entries must not restore before preflight completes"
        );
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "alpha");
    }

    #[test]
    fn delete_entries_empty_paths_short_circuits() {
        let dir = make_workspace();
        let results = delete_entries_blocking(
            dir.path(),
            CodingDeleteEntryRequest {
                root: workspace_str(&dir),
                paths: vec![],
            },
        )
        .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn delete_entries_rejects_workspace_escape() {
        let dir = make_workspace();
        let outside = tempfile::tempdir().unwrap();
        let req = CodingDeleteEntryRequest {
            root: workspace_str(&dir),
            paths: vec![outside
                .path()
                .join("nope.txt")
                .to_string_lossy()
                .into_owned()],
        };
        // Pre-resolution should reject before invoking trash.
        assert!(delete_entries_blocking(dir.path(), req).is_err());
    }

    #[test]
    fn batch_mutations_reject_duplicate_sources_before_changing_disk() {
        let dir = make_workspace();
        let root = dir.path();
        let source = root.join("a.txt").to_string_lossy().into_owned();
        let destination = root.join("dest");
        std::fs::create_dir(&destination).unwrap();

        let delete_error = delete_entries_blocking(
            root,
            CodingDeleteEntryRequest {
                root: workspace_str(&dir),
                paths: vec![source.clone(), source.clone()],
            },
        )
        .unwrap_err();
        assert!(delete_error.contains("重复"));
        assert!(root.join("a.txt").exists());

        let copy_error = copy_entries_blocking(
            root,
            CodingCopyEntryRequest {
                root: workspace_str(&dir),
                sources: vec![source.clone(), source.clone()],
                destination: destination.to_string_lossy().into_owned(),
            },
        )
        .unwrap_err();
        assert!(copy_error.contains("重复"));
        assert!(!destination.join("a.txt").exists());

        let move_error = move_entries_blocking(
            root,
            CodingMoveEntryRequest {
                root: workspace_str(&dir),
                sources: vec![source.clone(), source],
                destination: destination.to_string_lossy().into_owned(),
            },
        )
        .unwrap_err();
        assert!(move_error.contains("重复"));
        assert!(root.join("a.txt").exists());
    }

    #[test]
    fn copy_entries_creates_copies_and_preserves_basename() {
        let dir = make_workspace();
        let root = dir.path();
        let dest_dir = root.join("dest");
        std::fs::create_dir(&dest_dir).unwrap();

        let req = CodingCopyEntryRequest {
            root: workspace_str(&dir),
            sources: vec![
                root.join("a.txt").to_string_lossy().into_owned(),
                root.join("sub")
                    .join("b.txt")
                    .to_string_lossy()
                    .into_owned(),
            ],
            destination: dest_dir.to_string_lossy().into_owned(),
        };
        let results = copy_entries_blocking(root, req).expect("copy should run");
        assert_eq!(results.len(), 2);
        let canonical_destination = dest_dir.canonicalize().unwrap();
        for r in &results {
            assert!(r.ok, "expected ok, got {:?}", r);
            assert!(Path::new(&r.path).starts_with(&canonical_destination));
        }
        assert!(dest_dir.join("a.txt").exists());
        assert!(dest_dir.join("b.txt").exists());
        // source should still exist
        assert!(root.join("a.txt").exists());
        assert!(root.join("sub").join("b.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_directory_copy_removes_incomplete_target() {
        use std::os::unix::fs::symlink;

        let dir = make_workspace();
        let root = dir.path();
        let source = root.join("with-link");
        let destination = root.join("dest");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("first.txt"), "partial").unwrap();
        symlink(root.join("a.txt"), source.join("blocked-link")).unwrap();
        std::fs::create_dir(&destination).unwrap();

        let results = copy_entries_blocking(
            root,
            CodingCopyEntryRequest {
                root: workspace_str(&dir),
                sources: vec![source.to_string_lossy().into_owned()],
                destination: destination.to_string_lossy().into_owned(),
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].ok);
        assert!(!destination.join("with-link").exists());
    }

    #[test]
    fn copy_entries_rejects_directory_into_itself() {
        let dir = make_workspace();
        let root = dir.path();
        let req = CodingCopyEntryRequest {
            root: workspace_str(&dir),
            sources: vec![root.join("sub").to_string_lossy().into_owned()],
            destination: root
                .join("sub")
                .join("inner")
                .to_string_lossy()
                .into_owned(),
        };
        assert!(copy_entries_blocking(root, req).is_err());
    }

    #[test]
    fn move_entries_renames_in_place_within_same_dir() {
        let dir = make_workspace();
        let root = dir.path();
        let dest_dir = root.join("dest");
        std::fs::create_dir(&dest_dir).unwrap();

        let req = CodingMoveEntryRequest {
            root: workspace_str(&dir),
            sources: vec![root.join("a.txt").to_string_lossy().into_owned()],
            destination: dest_dir.to_string_lossy().into_owned(),
        };
        let results = move_entries_blocking(root, req).expect("move should run");
        assert_eq!(results.len(), 1);
        assert!(results[0].ok);
        assert!(!root.join("a.txt").exists());
        assert!(dest_dir.join("a.txt").exists());
    }

    #[test]
    fn copy_entries_rejects_path_outside_workspace() {
        let dir = make_workspace();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        let dest_dir = root.join("dest");
        std::fs::create_dir(&dest_dir).unwrap();
        let req = CodingCopyEntryRequest {
            root: workspace_str(&dir),
            sources: vec![outside
                .path()
                .join("foreign.txt")
                .to_string_lossy()
                .into_owned()],
            destination: dest_dir.to_string_lossy().into_owned(),
        };
        assert!(copy_entries_blocking(root, req).is_err());
    }
}
