//! Application-managed Git worktrees for concurrent coding tasks.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::coding::changeset;
use crate::coding::task;
use crate::coding::task::TaskPhase;
use crate::shell_fs::FilesystemAccess;

#[derive(Clone, Debug, Serialize, Deserialize)]
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

fn worktrees_dir() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("coding-worktrees")
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let mut command = crate::process_supervisor::background_sync_command("git");
    if arguments.first() == Some(&"status") {
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(crate::coding_workspace::read_only_git_overrides(root)?);
    }
    let output = command
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .map_err(|error| format!("无法启动 Git：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git 操作失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

pub fn integrate(root: &Path, task_id: &str) -> Result<IsolatedWorkspace, String> {
    let workspace = info(root).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    let set = changeset::load(root, task_id);
    let commit = set
        .committed_hash
        .ok_or_else(|| "请先提交任务变更，再应用到原项目".to_string())?;
    if task::list_tasks(Path::new(&workspace.source_root))
        .iter()
        .any(|entry| {
            matches!(
                entry.phase,
                TaskPhase::Discovering
                    | TaskPhase::Implementing
                    | TaskPhase::Verifying
                    | TaskPhase::Diagnosing
                    | TaskPhase::Repairing
            )
        })
    {
        return Err("原项目还有任务在执行，请等待其完成后再应用".into());
    }
    integrate_commit_in(root, &commit, &worktrees_dir())
}

fn integrate_commit_in(
    root: &Path,
    commit: &str,
    managed: &Path,
) -> Result<IsolatedWorkspace, String> {
    let mut workspace =
        info_in(root, managed).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    let source = PathBuf::from(&workspace.source_root);
    if !is_clean(&source)? {
        return Err("原项目有未提交变更；请先保存或提交，再应用隔离任务".into());
    }
    let source_head = git(&source, &["rev-parse", "HEAD"])?;
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
            &source_head,
        ],
    )?;
    let since = workspace
        .integrated_worktree_head
        .as_deref()
        .unwrap_or(&workspace.base_head);
    let commits = git(
        root,
        &["rev-list", "--reverse", &format!("{since}..{commit}")],
    )?;
    let commits = commits
        .lines()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let preview_result = if commits.is_empty() {
        Err("没有尚未回并的新提交".into())
    } else {
        let mut args = vec!["cherry-pick"];
        args.extend(commits);
        git(&preview, &args)
    };
    let preview_head = preview_result.and_then(|_| git(&preview, &["rev-parse", "HEAD"]));
    let _ = git(
        &source,
        &["worktree", "remove", "--force", &preview.to_string_lossy()],
    );
    let preview_head =
        preview_head.map_err(|error| format!("回并预检发现冲突，原项目未被修改：{error}"))?;
    if git(&source, &["rev-parse", "HEAD"])? != source_head || !is_clean(&source)? {
        return Err("预检期间原项目发生变化，请刷新后重试".into());
    }
    git(&source, &["merge", "--ff-only", &preview_head]).map_err(|error| {
        format!("原项目在应用时发生变化或 Git 拒绝快进，请检查状态后重试：{error}")
    })?;
    workspace.integrated_hash = Some(git(&source, &["rev-parse", "HEAD"])?);
    workspace.integrated_worktree_head = Some(commit.to_string());
    workspace.integration_verified = false;
    let manifest =
        manifest_path_in(root, managed).ok_or_else(|| "隔离工作树元数据丢失".to_string())?;
    crate::coding::store::write_json(&manifest, &workspace)?;
    Ok(workspace)
}

pub fn associate(root: &Path, task_id: &str, session_id: Option<&str>) -> Result<(), String> {
    let Some(mut workspace) = info(root) else {
        return Ok(());
    };
    if !workspace.task_ids.iter().any(|id| id == task_id) {
        workspace.task_ids.push(task_id.into());
    }
    if let Some(session) = session_id {
        if !workspace.session_ids.iter().any(|id| id == session) {
            workspace.session_ids.push(session.into());
        }
    }
    let manifest = manifest_path_in(root, &worktrees_dir())
        .ok_or_else(|| "隔离工作树元数据丢失".to_string())?;
    crate::coding::store::write_json(&manifest, &workspace)
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
        return Ok(());
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
pub async fn coding_isolation_integrate(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<IsolatedWorkspace, String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    let workspace = info(&root).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    access.require_workspace(&workspace.source_root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&workspace.source_root).await?;
    tokio::task::spawn_blocking(move || integrate(&root, &task_id))
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
        integrate_commit_in(Path::new(&first.root), &first_commit, managed.path()).unwrap();
        integrate_commit_in(Path::new(&second.root), &second_commit, managed.path()).unwrap();
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
        integrate_commit_in(Path::new(&first.root), &first_commit, managed.path()).unwrap();
        let before = git(source.path(), &["rev-parse", "HEAD"]).unwrap();
        assert!(
            integrate_commit_in(Path::new(&second.root), &second_commit, managed.path()).is_err()
        );
        assert_eq!(git(source.path(), &["rev-parse", "HEAD"]).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(source.path().join("a.txt")).unwrap(),
            "first\n"
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
        let integrated = integrate_commit_in(root, &second, managed.path()).unwrap();
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
        integrate_commit_in(root, &third, managed.path()).unwrap();
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
