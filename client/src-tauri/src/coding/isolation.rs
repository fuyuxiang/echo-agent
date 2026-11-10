//! Application-managed Git worktrees for concurrent coding tasks.

use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::coding::changeset;
use crate::coding::task;
use crate::coding::task::TaskPhase;
use crate::shell_fs::FilesystemAccess;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedWorkspace {
    pub root: String,
    pub source_root: String,
    pub base_head: String,
    #[serde(default)]
    pub integrated_hash: Option<String>,
    #[serde(default)]
    pub integrated_worktree_head: Option<String>,
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub start_includes_uncommitted: bool,
    /// A conflict-free integration still needs checks in the source project.
    #[serde(default)]
    pub integration_verified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsolatedIntegrationPlan {
    source_root: String,
    source_head: String,
    source_ref: String,
    commits: Vec<IsolatedIntegrationCommit>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct IsolatedIntegrationCommit {
    hash: String,
    subject: String,
    files: Vec<String>,
}

fn worktrees_dir() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("coding-worktrees")
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = crate::process_supervisor::background_sync_command("git");
    if arguments.first() == Some(&"status") {
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(crate::coding_workspace::read_only_git_overrides(root)?);
    }
    run_git(command.arg("-C").arg(root).args(arguments))
}

fn run_git(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|error| format!("无法启动 Git：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git 操作失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git_output(root, arguments)?)
        .trim()
        .to_string())
}

fn manifest_path_in(root: &Path, managed: &Path) -> Option<PathBuf> {
    let base = managed.canonicalize().ok()?;
    let canonical = root.canonicalize().ok()?;
    let parent = canonical.parent()?;
    if !parent.starts_with(&base)
        || parent.parent()? != base
        || canonical != parent.join("worktree")
    {
        return None;
    }
    Some(parent.join("manifest.json"))
}

pub fn managed_worktrees() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(worktrees_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("worktree"))
        .filter(|root| info(root).is_some())
        .collect()
}

pub fn info(root: &Path) -> Option<IsolatedWorkspace> {
    info_in(root, &worktrees_dir())
}

fn info_in(root: &Path, managed: &Path) -> Option<IsolatedWorkspace> {
    let manifest = manifest_path_in(root, managed)?;
    let workspace: IsolatedWorkspace = crate::coding::store::read_json(&manifest)?;
    let canonical = root.canonicalize().ok()?;
    if workspace.root != canonical.to_string_lossy() || !canonical.join(".git").is_file() {
        return None;
    }
    Some(workspace)
}

pub fn create(root: &Path) -> Result<IsolatedWorkspace, String> {
    create_in(root, &worktrees_dir())
}

fn create_in(root: &Path, managed: &Path) -> Result<IsolatedWorkspace, String> {
    let source = info_in(root, managed)
        .map(|workspace| PathBuf::from(workspace.source_root))
        .unwrap_or_else(|| root.to_path_buf());
    let source = source
        .canonicalize()
        .map_err(|error| format!("无法解析原项目：{error}"))?;
    let top = git(&source, &["rev-parse", "--show-toplevel"])?;
    if Path::new(&top).canonicalize().ok().as_deref() != Some(source.as_path()) {
        return Err("并行任务需要从 Git 仓库根目录启动".into());
    }
    let base_head = git(&source, &["rev-parse", "HEAD"])?;
    let container = managed.join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&container).map_err(|error| format!("创建隔离目录失败：{error}"))?;
    let target = container.join("worktree");
    if let Err(error) = git(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            "--",
            &target.to_string_lossy(),
            &base_head,
        ],
    ) {
        let _ = std::fs::remove_dir(&container);
        return Err(error);
    }
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("无法解析隔离工作树：{error}"))?;
    let workspace = IsolatedWorkspace {
        root: canonical.to_string_lossy().into_owned(),
        source_root: source.to_string_lossy().into_owned(),
        base_head,
        integrated_hash: None,
        integrated_worktree_head: None,
        task_ids: Vec::new(),
        session_ids: Vec::new(),
        created_at: chrono::Utc::now().to_rfc3339(),
        start_includes_uncommitted: false,
        integration_verified: false,
    };
    if let Err(error) =
        crate::coding::store::write_json(&container.join("manifest.json"), &workspace)
    {
        let _ = git(&source, &["worktree", "remove", "--force", &workspace.root]);
        return Err(error);
    }
    Ok(workspace)
}

fn is_clean(root: &Path) -> Result<bool, String> {
    Ok(git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty())
}

fn git_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = git(root, &["rev-parse", "--git-path", name])?;
    Ok(root.join(path))
}

/// Use Git's real index lock to prevent checkout/staging during installation.
/// The detached helper writes a separate index while the source stays locked.
struct IntegrationIndex {
    path: PathBuf,
    lock: PathBuf,
    original: std::fs::File,
    permissions: std::fs::Permissions,
    checkout: tempfile::TempPath,
}

impl IntegrationIndex {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = git_path(root, "index")?;
        // The real index name can be overridden by Git; always append ".lock".
        let mut lock_name = path.as_os_str().to_os_string();
        lock_name.push(".lock");
        let lock = PathBuf::from(lock_name);
        let directory = path.parent().ok_or("无法确认 Git 暂存区目录")?;
        let mut checkout = tempfile::Builder::new()
            .prefix("echo-integration-index-")
            .tempfile_in(directory)
            .map_err(|error| format!("无法创建临时 Git 暂存区：{error}"))?;
        let guard = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock)
            .map_err(|error| format!("Git 暂存区正在被其他操作占用，请稍后重试：{error}"))?;
        drop(guard);
        // Keep the rollback copy independent of the live index. Windows cannot
        // replace the live index while our read handle is still open.
        let snapshot = (|| -> std::io::Result<_> {
            let mut live = std::fs::File::open(&path)?;
            let permissions = live.metadata()?.permissions();
            let mut original = tempfile::tempfile_in(directory)?;
            std::io::copy(&mut live, &mut original)?;
            original.rewind()?;
            Ok((original, permissions))
        })();
        let (original, permissions) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = std::fs::remove_file(&lock);
                return Err(format!("无法备份 Git 暂存区：{error}"));
            }
        };
        let copied = std::io::copy(&mut &original, checkout.as_file_mut());
        let index = Self {
            path,
            lock,
            original,
            permissions,
            checkout: checkout.into_temp_path(),
        };
        copied.map_err(|error| format!("无法复制 Git 暂存区：{error}"))?;
        if !git(root, &["rev-parse", "--shared-index-path"])?.is_empty() {
            run_git(
                crate::process_supervisor::background_sync_command("git")
                    .arg("-C")
                    .arg(root)
                    .env("GIT_INDEX_FILE", index.git_checkout_path(root))
                    .args([
                        "-c",
                        "core.splitIndex=false",
                        "update-index",
                        "--no-split-index",
                    ]),
            )?;
        }
        Ok(index)
    }

    fn git_checkout_path(&self, root: &Path) -> &Path {
        // Git runs with `-C root`. A relative path avoids the verbatim
        // Windows prefix, including when the project path is long.
        // A linked worktree's index can live outside its root.
        self.checkout
            .strip_prefix(root)
            .unwrap_or_else(|_| dunce::simplified(&self.checkout))
    }

    fn write_index(&self, bytes: &mut impl Read) -> Result<(), String> {
        let write = (|| -> std::io::Result<tempfile::TempPath> {
            let mut temporary = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
            std::io::copy(bytes, &mut temporary)?;
            temporary
                .as_file()
                .set_permissions(self.permissions.clone())?;
            temporary.as_file().sync_all()?;
            Ok(temporary.into_temp_path())
        })();
        let temporary = write.map_err(|error| format!("无法写入临时 Git 暂存区：{error}"))?;
        temporary
            .persist(&self.path)
            .map_err(|error| format!("无法替换 Git 暂存区：{}", error.error))
    }

    fn install(&self) -> Result<(), String> {
        let mut checkout = std::fs::File::open(&self.checkout)
            .map_err(|error| format!("无法读取回并后的 Git 暂存区：{error}"))?;
        self.write_index(&mut checkout)
    }

    fn restore(&self) -> Result<(), String> {
        let mut original = &self.original;
        original.rewind().map_err(|error| error.to_string())?;
        self.write_index(&mut original)
    }
}

impl Drop for IntegrationIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// Git owns the reference locks. The branch and expected old commit are part
/// of the actual write, and the worktree can be checked while they are held.
struct IntegrationRefs {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    errors: tempfile::NamedTempFile,
}

impl IntegrationRefs {
    fn prepare(root: &Path, plan: &IsolatedIntegrationPlan, next: &str) -> Result<Self, String> {
        let errors = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        let mut child = crate::process_supervisor::background_sync_command("git")
            .arg("-C")
            .arg(root)
            .args([
                "update-ref",
                "-m",
                "EchoAgent: integrate isolated task",
                "--stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                errors.reopen().map_err(|error| error.to_string())?,
            ))
            .spawn()
            .map_err(|error| format!("无法准备 Git 引用更新：{error}"))?;
        let input = child.stdin.take().expect("piped stdin");
        let output = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut refs = Self {
            child,
            input: Some(input),
            output,
            errors,
        };
        refs.send("start")?;
        refs.expect("start: ok")?;
        if plan.source_ref == "HEAD" {
            refs.send("option no-deref")?;
        }
        refs.send(&format!(
            "update {} {next} {}",
            plan.source_ref, plan.source_head
        ))?;
        refs.send(&format!("update ORIG_HEAD {}", plan.source_head))?;
        refs.send("prepare")?;
        refs.expect("prepare: ok")?;
        Ok(refs)
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.input.as_mut().expect("transaction stdin"), "{line}")
            .map_err(|error| format!("无法写入 Git 引用事务：{error}"))
    }

    fn expect(&mut self, expected: &str) -> Result<(), String> {
        let mut line = String::new();
        self.output
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if line.trim_end() == expected {
            return Ok(());
        }
        let detail = std::fs::read_to_string(self.errors.path()).unwrap_or_default();
        Err(format!("Git 拒绝更新原项目引用：{}", detail.trim()))
    }

    fn commit(&mut self) -> Result<(), String> {
        self.send("commit")?;
        self.expect("commit: ok")
    }
}

impl Drop for IntegrationRefs {
    fn drop(&mut self) {
        // EOF aborts an uncommitted update-ref transaction.
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

fn ensure_source_revision(source: &Path, plan: &IsolatedIntegrationPlan) -> Result<(), String> {
    if git(source, &["rev-parse", "HEAD"])? != plan.source_head
        || git(source, &["rev-parse", "--symbolic-full-name", "HEAD"])? != plan.source_ref
    {
        return Err("预检期间原项目发生变化，请刷新后重试".into());
    }
    Ok(())
}

fn install_integration(
    source: &Path,
    preview: &Path,
    plan: &IsolatedIntegrationPlan,
    next: &str,
) -> Result<(), String> {
    let hooks = tempfile::tempdir().map_err(|error| error.to_string())?;
    let no_hooks = format!("core.hooksPath={}", hooks.path().display());
    let gitdir = git(preview, &["rev-parse", "--absolute-git-dir"])?;
    git(
        preview,
        &[
            "-c",
            &no_hooks,
            "update-ref",
            "--no-deref",
            "HEAD",
            &plan.source_head,
            next,
        ],
    )?;
    let index = IntegrationIndex::acquire(source)?;
    ensure_source_available(source)?;
    // The helper uses the same object database. Mirror per-worktree checkout
    // settings so sparse paths and filters have the original worktree policy.
    for name in ["config.worktree", "info/sparse-checkout"] {
        let from = git_path(source, name)?;
        if from.is_file() {
            let to = git_path(preview, name)?;
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::copy(from, to).map_err(|error| error.to_string())?;
        }
    }
    let mut refs = IntegrationRefs::prepare(source, plan, next)?;
    ensure_source_revision(source, plan)?;
    let checkout = |arguments: &[&str]| {
        run_git(
            crate::process_supervisor::background_sync_command("git")
                .arg("-C")
                .arg(source)
                .args(["--git-dir", &gitdir, "--work-tree"])
                .arg(source)
                .args(["-c", &no_hooks, "-c", "core.splitIndex=false"])
                .env("GIT_INDEX_FILE", index.git_checkout_path(source))
                .args(arguments),
        )
    };
    // Do the working-tree update with Git's no-clobber checkout. Its HEAD is
    // detached in the helper, so it cannot accidentally advance a live branch.
    checkout(&["merge", "--ff-only", "--no-overwrite-ignore", next])?;
    let restore_worktree = || {
        checkout(&[
            "checkout",
            "--no-overwrite-ignore",
            "--detach",
            &plan.source_head,
        ])
    };
    if let Err(error) = index.install() {
        return match restore_worktree() {
            Ok(_) => Err(error),
            Err(restore) => Err(format!("{error}。无法自动恢复原工作区：{restore}")),
        };
    }
    if let Err(error) = refs.commit() {
        return match (restore_worktree(), index.restore()) {
            (Ok(_), Ok(())) => Err(error),
            (worktree, index) => Err(format!(
                "{error}。无法完整恢复原工作区：{}",
                worktree
                    .err()
                    .into_iter()
                    .chain(index.err())
                    .collect::<Vec<_>>()
                    .join("；")
            )),
        };
    }
    drop(refs);
    drop(index);
    if let Err(error) = git(
        source,
        &["hook", "run", "--ignore-missing", "post-merge", "--", "0"],
    ) {
        tracing::warn!(%error, "isolated task post-merge hook did not complete");
    }
    Ok(())
}

fn task_commit(root: &Path, task_id: &str) -> Result<String, String> {
    crate::coding::store::validate_task_id(task_id)?;
    changeset::load(root, task_id)
        .committed_hash
        .ok_or_else(|| "请先提交任务变更，再应用到原项目".to_string())
}

fn ensure_source_available(source: &Path) -> Result<(), String> {
    if task::list_tasks(source).iter().any(|entry| {
        matches!(
            entry.phase,
            TaskPhase::Discovering
                | TaskPhase::Implementing
                | TaskPhase::Verifying
                | TaskPhase::Diagnosing
                | TaskPhase::Repairing
        )
    }) {
        return Err("原项目还有任务在执行，请等待其完成后再应用".into());
    }
    if !is_clean(source)? {
        return Err("原项目有未提交变更；请先保存或提交，再应用隔离任务".into());
    }
    Ok(())
}

fn integration_plan(
    root: &Path,
    commit: &str,
    workspace: &IsolatedWorkspace,
) -> Result<IsolatedIntegrationPlan, String> {
    let source = Path::new(&workspace.source_root);
    ensure_source_available(source)?;
    let since = workspace
        .integrated_worktree_head
        .as_deref()
        .unwrap_or(&workspace.base_head);
    for hash in [since, commit] {
        if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("隔离工作树提交标识无效".into());
        }
    }
    git(root, &["merge-base", "--is-ancestor", since, commit])
        .and_then(|_| git(root, &["merge-base", "--is-ancestor", commit, "HEAD"]))
        .map_err(|_| "任务提交已不在可应用的隔离工作树历史中".to_string())?;
    let revisions = git(
        root,
        &["rev-list", "--reverse", &format!("{since}..{commit}")],
    )?;
    let commits = revisions
        .lines()
        .map(|hash| {
            let subject = git(root, &["show", "--no-patch", "--format=%s", hash])?;
            let files = git_output(
                root,
                &[
                    "-c",
                    "core.quotePath=false",
                    "diff-tree",
                    "--root",
                    "-r",
                    "--no-commit-id",
                    "--name-only",
                    "-z",
                    hash,
                ],
            )?;
            let files = std::str::from_utf8(&files)
                .map_err(|_| "待应用提交含无法安全显示的文件名，请在 Git 中手工审阅".to_string())?;
            Ok(IsolatedIntegrationCommit {
                hash: hash.into(),
                subject,
                files: files.split_terminator('\0').map(str::to_string).collect(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if commits.is_empty() {
        return Err("没有尚未回并的新提交".into());
    }
    Ok(IsolatedIntegrationPlan {
        source_root: workspace.source_root.clone(),
        source_head: git(source, &["rev-parse", "HEAD"])?,
        source_ref: git(source, &["rev-parse", "--symbolic-full-name", "HEAD"])?,
        commits,
    })
}

fn integrate_commit_in(
    root: &Path,
    commit: &str,
    managed: &Path,
    expected: &IsolatedIntegrationPlan,
) -> Result<IsolatedWorkspace, String> {
    let workspace = info_in(root, managed).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    let manifest =
        manifest_path_in(root, managed).ok_or_else(|| "隔离工作树元数据丢失".to_string())?;
    let source = PathBuf::from(&workspace.source_root);
    let plan = integration_plan(root, commit, &workspace)?;
    if &plan != expected {
        return Err("原项目或待应用提交已变化，请重新预览并确认".into());
    }
    let temp = tempfile::tempdir().map_err(|error| format!("创建合并预检目录失败：{error}"))?;
    let preview = temp.path().join("merge-preview");
    git(
        &source,
        &[
            "worktree",
            "add",
            "--detach",
            "--",
            &preview.to_string_lossy(),
            &plan.source_head,
        ],
    )?;
    let mut args = vec!["cherry-pick"];
    args.extend(plan.commits.iter().map(|entry| entry.hash.as_str()));
    let preview_result = git(&preview, &args);
    let preview_head = preview_result
        .and_then(|_| git(&preview, &["rev-parse", "HEAD"]))
        .map_err(|error| format!("回并预检发现冲突，原项目未被修改：{error}"));
    let installed = preview_head.and_then(|head| {
        install_integration(&source, &preview, &plan, &head)?;
        Ok(head)
    });
    let _ = git(
        &source,
        &["worktree", "remove", "--force", &preview.to_string_lossy()],
    );
    let preview_head = installed?;
    crate::coding::store::update_json(&manifest, |current: &mut IsolatedWorkspace| {
        if current.root != workspace.root
            || current.source_root != workspace.source_root
            || current.integrated_worktree_head != workspace.integrated_worktree_head
        {
            return Err("隔离工作树管理状态已变化".into());
        }
        current.integrated_hash = Some(preview_head.clone());
        current.integrated_worktree_head = Some(commit.to_string());
        current.integration_verified = false;
        Ok(current.clone())
    })
    .map_err(|error| {
        format!("原项目已应用至 {preview_head}，但无法保存回并状态；请核对原项目的提交：{error}")
    })
}

pub fn associate(root: &Path, task_id: &str, session_id: Option<&str>) -> Result<(), String> {
    let Some(workspace) = info(root) else {
        return Ok(());
    };
    let manifest = manifest_path_in(root, &worktrees_dir())
        .ok_or_else(|| "隔离工作树元数据丢失".to_string())?;
    crate::coding::store::update_json(&manifest, |current: &mut IsolatedWorkspace| {
        if current.root != workspace.root || current.source_root != workspace.source_root {
            return Err("隔离工作树管理状态已变化".into());
        }
        if !current.task_ids.iter().any(|id| id == task_id) {
            current.task_ids.push(task_id.into());
        }
        if let Some(session) = session_id {
            if !current.session_ids.iter().any(|id| id == session) {
                current.session_ids.push(session.into());
            }
        }
        Ok(())
    })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ManagedWorkspaceStatus {
    pub root: String,
    pub source_root: String,
    pub status: String,
    pub detail: Option<String>,
    pub task_ids: Vec<String>,
    pub session_ids: Vec<String>,
}

fn statuses(source: &Path) -> Vec<ManagedWorkspaceStatus> {
    let Ok(entries) = std::fs::read_dir(worktrees_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let manifest = entry.path().join("manifest.json");
            let workspace = match crate::coding::store::read_json::<IsolatedWorkspace>(&manifest) {
                Some(workspace) => workspace,
                None if manifest.exists() => {
                    return Some(ManagedWorkspaceStatus {
                        root: entry.path().join("worktree").to_string_lossy().into_owned(),
                        source_root: String::new(),
                        status: "damaged".into(),
                        detail: Some(
                            "管理元数据损坏，无法确认原项目归属；已保留全部资源，不能自动清理"
                                .into(),
                        ),
                        task_ids: Vec::new(),
                        session_ids: Vec::new(),
                    })
                }
                None => return None,
            };
            if Path::new(&workspace.source_root) != source && Path::new(&workspace.root) != source {
                return None;
            }
            let root = Path::new(&workspace.root);
            let (status, detail) = if !root.exists() {
                (
                    "missing",
                    Some("工作树目录已丢失或被移动；保留元数据以便诊断".into()),
                )
            } else if info(root).is_none() {
                (
                    "damaged",
                    Some("Git 链接或管理元数据无效；不会自动删除".into()),
                )
            } else if !is_clean(root).unwrap_or(false) {
                ("dirty", Some("含未提交修改，清理会被拒绝".into()))
            } else {
                ("ready", None)
            };
            Some(ManagedWorkspaceStatus {
                root: workspace.root,
                source_root: workspace.source_root,
                status: status.into(),
                detail,
                task_ids: workspace.task_ids,
                session_ids: workspace.session_ids,
            })
        })
        .collect()
}

fn ensure_no_cwd_process(root: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let entries =
            std::fs::read_dir("/proc").map_err(|error| format!("无法确认进程占用：{error}"))?;
        for entry in entries.flatten() {
            if let Ok(cwd) = std::fs::read_link(entry.path().join("cwd")) {
                if cwd.starts_with(root) {
                    return Err("工作树仍有终端或其他进程占用，请先结束后重试".into());
                }
            }
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let output = crate::process_supervisor::background_sync_command("lsof")
            .args(["-Fn", "+D"])
            .arg(root)
            .output()
            .map_err(|error| format!("无法确认进程占用，未执行清理：{error}"))?;
        // lsof exits 1 with no diagnostic when no file descriptors reference
        // the tree. Any diagnostic leaves occupancy uncertain and fails closed.
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(());
        }
        if !output.status.success() || !output.stderr.is_empty() {
            return Err("无法完整确认进程占用，未执行清理".into());
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(path) = line.strip_prefix('n') {
                if Path::new(path).starts_with(root) {
                    return Err("工作树仍有终端、编辑器或其他进程占用，请先关闭后重试".into());
                }
            }
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Err("当前平台尚不能安全核对工作树进程占用；请使用 Git worktree remove 手工清理".into())
}

fn cleanup_in(root: &Path, managed: &Path, check_processes: bool) -> Result<(), String> {
    let workspace = info_in(root, managed)
        .ok_or_else(|| "仅允许清理由本应用管理且状态完整的工作树".to_string())?;
    let source = PathBuf::from(&workspace.source_root);
    if task::list_tasks(root).iter().any(|task| {
        matches!(
            task.phase,
            TaskPhase::Discovering
                | TaskPhase::Implementing
                | TaskPhase::Verifying
                | TaskPhase::Repairing
                | TaskPhase::Diagnosing
        )
    }) {
        return Err("工作树仍有关联任务在执行，请先停止所有运行".into());
    }
    if !git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
        ],
    )?
    .is_empty()
    {
        return Err("工作树含未提交或忽略文件，已保留目录；请检查并处理后重试".into());
    }
    let head = git(root, &["rev-parse", "HEAD"])?;
    if head != workspace.base_head
        && workspace.integrated_worktree_head.as_deref() != Some(head.as_str())
    {
        return Err("工作树仍有尚未回并的提交，已拒绝清理".into());
    }
    if let Some(integrated) = &workspace.integrated_hash {
        git(
            &source,
            &["merge-base", "--is-ancestor", integrated, "HEAD"],
        )
        .map_err(|_| "原项目已不再包含回并结果，已拒绝清理".to_string())?;
    }
    if check_processes {
        ensure_no_cwd_process(root)?;
    }
    let manifest = manifest_path_in(root, managed).ok_or_else(|| "管理元数据丢失".to_string())?;
    // Never force deletion. Git checks registration, locks and dirty state
    // again at the destructive boundary; session records remain separate.
    git(&source, &["worktree", "remove", "--", &workspace.root])?;
    std::fs::remove_file(&manifest)
        .map_err(|error| format!("工作树已清理，移除元数据失败：{error}"))?;
    if let Some(parent) = manifest.parent() {
        match std::fs::remove_file(parent.join("manifest.json.lock")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("工作树已清理，移除管理锁文件失败：{error}")),
        }
        std::fs::remove_dir(parent)
            .map_err(|error| format!("工作树已清理，管理目录含其他资源，已保留：{error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn coding_isolation_list(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<Vec<ManagedWorkspaceStatus>, String> {
    let root = access.require_workspace(&root)?;
    Ok(statuses(&root))
}

#[tauri::command]
pub async fn coding_isolation_cleanup(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<(), String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    let workspace = info(&root).ok_or_else(|| "当前项目不是受管理工作树".to_string())?;
    access.require_workspace(&workspace.source_root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&workspace.source_root).await?;
    tokio::task::spawn_blocking(move || cleanup_in(&root, &worktrees_dir(), true))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn coding_isolation_create(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<IsolatedWorkspace, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    let workspace = tokio::task::spawn_blocking(move || create(&root))
        .await
        .map_err(|error| format!("创建隔离工作树失败：{error}"))??;
    access.authorize_workspace(&workspace.root)?;
    Ok(workspace)
}

#[tauri::command]
pub async fn coding_isolation_info(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<Option<IsolatedWorkspace>, String> {
    let root = access.require_workspace(&root)?;
    Ok(info(&root))
}

#[tauri::command]
pub async fn coding_isolation_integration_preview(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<IsolatedIntegrationPlan, String> {
    let root = access.require_workspace(&root)?;
    let workspace = info(&root).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    access.require_workspace(&workspace.source_root)?;
    tokio::task::spawn_blocking(move || {
        let commit = task_commit(&root, &task_id)?;
        integration_plan(&root, &commit, &workspace)
    })
    .await
    .map_err(|error| format!("读取待应用提交失败：{error}"))?
}

#[tauri::command]
pub async fn coding_isolation_integrate(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    expected: IsolatedIntegrationPlan,
) -> Result<IsolatedWorkspace, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    let workspace = info(&root).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    let source = access.require_workspace(&workspace.source_root)?;
    if workspace.source_root != expected.source_root {
        return Err("原项目已变化，请重新预览并确认".into());
    }
    crate::coding_document_bridge::ensure_workspace_admission(&workspace.source_root).await?;
    tokio::task::spawn_blocking(move || {
        crate::coding::store::with_workspace_transaction(&source, || {
            let commit = task_commit(&root, &task_id)?;
            integrate_commit_in(&root, &commit, &worktrees_dir(), &expected)
        })
    })
    .await
    .map_err(|error| format!("应用隔离任务失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]).unwrap();
        git(dir.path(), &["config", "user.name", "Echo Test"]).unwrap();
        git(dir.path(), &["config", "user.email", "echo@example.test"]).unwrap();
        git(dir.path(), &["config", "core.autocrlf", "false"]).unwrap();
        std::fs::write(dir.path().join("a.txt"), "base a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "base b\n").unwrap();
        git(dir.path(), &["add", "."]).unwrap();
        git(dir.path(), &["commit", "-qm", "baseline"]).unwrap();
        dir
    }

    fn commit_change(root: &Path, path: &str, content: &str) -> String {
        std::fs::write(root.join(path), content).unwrap();
        git(root, &["add", "--", path]).unwrap();
        git(root, &["commit", "-qm", "task change"]).unwrap();
        git(root, &["rev-parse", "HEAD"]).unwrap()
    }

    fn apply_current_plan(
        root: &Path,
        commit: &str,
        managed: &Path,
    ) -> Result<IsolatedWorkspace, String> {
        let workspace = info_in(root, managed).unwrap();
        let plan = integration_plan(root, commit, &workspace)?;
        integrate_commit_in(root, commit, managed, &plan)
    }

    #[test]
    fn independent_worktrees_merge_without_overwriting_each_other() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let first = create_in(source.path(), managed.path()).unwrap();
        let second = create_in(source.path(), managed.path()).unwrap();
        assert_ne!(first.root, second.root);
        assert_eq!(
            info_in(Path::new(&first.root), managed.path())
                .unwrap()
                .base_head,
            first.base_head
        );
        let first_commit = commit_change(Path::new(&first.root), "a.txt", "task a\n");
        let second_commit = commit_change(Path::new(&second.root), "b.txt", "task b\n");
        apply_current_plan(Path::new(&first.root), &first_commit, managed.path()).unwrap();
        apply_current_plan(Path::new(&second.root), &second_commit, managed.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
            "task a\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.path().join("b.txt")).unwrap(),
            "task b\n"
        );
        assert!(info_in(Path::new(&second.root), managed.path())
            .unwrap()
            .integrated_hash
            .is_some());
    }

    #[test]
    fn conflicting_integration_keeps_source_unchanged() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let first = create_in(source.path(), managed.path()).unwrap();
        let second = create_in(source.path(), managed.path()).unwrap();
        let first_commit = commit_change(Path::new(&first.root), "a.txt", "first\n");
        let second_commit = commit_change(Path::new(&second.root), "a.txt", "second\n");
        apply_current_plan(Path::new(&first.root), &first_commit, managed.path()).unwrap();
        let before = git(source.path(), &["rev-parse", "HEAD"]).unwrap();
        assert!(
            apply_current_plan(Path::new(&second.root), &second_commit, managed.path()).is_err()
        );
        assert_eq!(git(source.path(), &["rev-parse", "HEAD"]).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
            "first\n"
        );
        assert!(is_clean(source.path()).unwrap());
    }

    #[test]
    fn ignored_local_file_is_never_overwritten_during_integration() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let workspace = create_in(source.path(), managed.path()).unwrap();
        let root = Path::new(&workspace.root);
        std::fs::write(
            git_path(source.path(), "info/exclude").unwrap(),
            "local.txt\n",
        )
        .unwrap();
        std::fs::write(source.path().join("local.txt"), "private local file\n").unwrap();
        std::fs::write(root.join("local.txt"), "isolated task version\n").unwrap();
        git(root, &["add", "-f", "--", "local.txt"]).unwrap();
        git(root, &["commit", "-qm", "add local name"]).unwrap();
        let next = git(root, &["rev-parse", "HEAD"]).unwrap();
        assert!(is_clean(source.path()).unwrap());

        let error = apply_current_plan(root, &next, managed.path()).unwrap_err();
        assert!(error.contains("local.txt"), "{error}");
        assert_eq!(
            std::fs::read_to_string(source.path().join("local.txt")).unwrap(),
            "private local file\n"
        );
        assert_eq!(
            git(source.path(), &["rev-parse", "HEAD"]).unwrap(),
            workspace.base_head
        );
        assert!(is_clean(source.path()).unwrap());
        assert!(!git_path(source.path(), "index")
            .unwrap()
            .with_extension("lock")
            .exists());
        assert!(info_in(root, managed.path())
            .unwrap()
            .integrated_hash
            .is_none());
    }

    #[test]
    fn helper_index_can_change_while_the_source_index_stays_locked() {
        let source = repository();
        let root = source.path().canonicalize().unwrap();
        let index = IntegrationIndex::acquire(&root).unwrap();
        let remove = ["update-index", "--force-remove", "--", "a.txt"];
        assert!(git(&root, &remove).is_err());
        run_git(
            crate::process_supervisor::background_sync_command("git")
                .arg("-C")
                .arg(&root)
                .env("GIT_INDEX_FILE", index.git_checkout_path(&root))
                .args(remove),
        )
        .unwrap();
        assert_eq!(git(&root, &["ls-files", "--", "a.txt"]).unwrap(), "a.txt");
        index.install().unwrap();
        assert!(git(&root, &["ls-files", "--", "a.txt"]).unwrap().is_empty());
        index.restore().unwrap();
        assert_eq!(git(&root, &["ls-files", "--", "a.txt"]).unwrap(), "a.txt");
        drop(index);
        git(&root, &remove).unwrap();
    }

    #[test]
    fn preparing_original_ref_prevents_checkout_of_a_branch_at_the_same_commit() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let workspace = create_in(source.path(), managed.path()).unwrap();
        let root = Path::new(&workspace.root);
        let next = commit_change(root, "a.txt", "isolated\n");
        let plan = integration_plan(root, &next, &workspace).unwrap();
        git(source.path(), &["branch", "other-branch"]).unwrap();
        {
            let _index = IntegrationIndex::acquire(source.path()).unwrap();
            let _refs = IntegrationRefs::prepare(source.path(), &plan, &next).unwrap();
            ensure_source_revision(source.path(), &plan).unwrap();
            assert!(git(source.path(), &["checkout", "other-branch"]).is_err());
            assert!(git(
                source.path(),
                &["symbolic-ref", "HEAD", "refs/heads/other-branch"]
            )
            .is_err());
        }
        assert_eq!(
            git(source.path(), &["rev-parse", "other-branch"]).unwrap(),
            plan.source_head
        );
        assert_eq!(
            git(source.path(), &["rev-parse", "HEAD"]).unwrap(),
            plan.source_head
        );
        git(source.path(), &["checkout", "other-branch"]).unwrap();
        // Even if the exact old commit is still current, the original plan is
        // not authorized to alter the newly selected branch or working tree.
        assert!(integrate_commit_in(root, &next, managed.path(), &plan).is_err());
        assert!(is_clean(source.path()).unwrap());
    }

    #[test]
    fn integration_reads_an_existing_split_index() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let workspace = create_in(source.path(), managed.path()).unwrap();
        git(source.path(), &["config", "core.splitIndex", "true"]).unwrap();
        git(source.path(), &["update-index", "--split-index"]).unwrap();
        assert!(!git(source.path(), &["rev-parse", "--shared-index-path"])
            .unwrap()
            .is_empty());
        let root = Path::new(&workspace.root);
        let next = commit_change(root, "a.txt", "split-index update\n");
        apply_current_plan(root, &next, managed.path()).unwrap();
        assert!(is_clean(source.path()).unwrap());
    }

    #[test]
    fn integration_preserves_sparse_worktree_paths() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("hidden")).unwrap();
        commit_change(source.path(), "hidden/file.txt", "initial\n");
        let workspace = create_in(source.path(), managed.path()).unwrap();
        git(
            source.path(),
            &["sparse-checkout", "set", "--no-cone", "/a.txt"],
        )
        .unwrap();
        assert!(!source.path().join("hidden").exists());
        let root = Path::new(&workspace.root);
        let next = commit_change(root, "hidden/file.txt", "task updated\n");
        apply_current_plan(root, &next, managed.path()).unwrap();
        assert!(!source.path().join("hidden").exists());
        assert_eq!(
            git(source.path(), &["show", "HEAD:hidden/file.txt"]).unwrap(),
            "task updated"
        );
        assert!(is_clean(source.path()).unwrap());
    }
    #[test]
    fn cleanup_refuses_dirty_and_unmerged_worktrees_then_removes_only_integrated_resource() {
        let source = repository();
        let managed = tempfile::tempdir().unwrap();
        let workspace = create_in(source.path(), managed.path()).unwrap();
        let root = Path::new(&workspace.root);
        assert!(!workspace.start_includes_uncommitted);
        std::fs::write(root.join("a.txt"), "uncommitted\n").unwrap();
        assert!(cleanup_in(root, managed.path(), false)
            .unwrap_err()
            .contains("未提交"));
        let first = commit_change(root, "a.txt", "first\n");
        assert!(cleanup_in(root, managed.path(), false)
            .unwrap_err()
            .contains("尚未回并"));
        let second = commit_change(root, "b.txt", "second\n");
        assert_ne!(first, second);
        let plan = integration_plan(root, &second, &workspace).unwrap();
        assert_eq!(plan.commits.len(), 2);
        assert_eq!(plan.commits[0].hash, first);
        assert_eq!(plan.commits[0].files, vec!["a.txt"]);
        assert_eq!(plan.commits[1].hash, second);
        assert_eq!(plan.commits[1].files, vec!["b.txt"]);
        let mut incomplete = plan.clone();
        incomplete.commits.remove(0);
        assert!(
            integrate_commit_in(root, &second, managed.path(), &incomplete)
                .unwrap_err()
                .contains("重新预览")
        );
        assert_eq!(
            git(source.path(), &["rev-parse", "HEAD"]).unwrap(),
            plan.source_head
        );
        git(source.path(), &["checkout", "-qb", "other-branch"]).unwrap();
        assert!(integrate_commit_in(root, &second, managed.path(), &plan)
            .unwrap_err()
            .contains("重新预览"));
        assert_eq!(
            git(source.path(), &["rev-parse", "HEAD"]).unwrap(),
            plan.source_head
        );
        let integrated = apply_current_plan(root, &second, managed.path()).unwrap();
        assert!(!integrated.integration_verified);
        assert_eq!(
            std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
            "first\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.path().join("b.txt")).unwrap(),
            "second\n"
        );
        let third = commit_change(root, "a.txt", "third\n");
        apply_current_plan(root, &third, managed.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
            "third\n"
        );
        cleanup_in(root, managed.path(), false).unwrap();
        assert!(!root.exists());
        assert!(!git(source.path(), &["worktree", "list", "--porcelain"])
            .unwrap()
            .contains(&workspace.root));
        assert!(source.path().join("a.txt").exists());
    }
}
