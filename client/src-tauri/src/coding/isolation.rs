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
}

fn worktrees_dir() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("coding-worktrees")
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = crate::process_supervisor::background_sync_command("git")
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
    if workspace.integrated_hash.is_some() {
        return Err("这个隔离任务已经应用到原项目".into());
    }
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
    if workspace.integrated_hash.is_some() {
        return Err("这个隔离任务已经应用到原项目".into());
    }
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
    let preview_result = git(&preview, &["cherry-pick", commit]);
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
    let manifest =
        manifest_path_in(root, managed).ok_or_else(|| "隔离工作树元数据丢失".to_string())?;
    crate::coding::store::write_json(&manifest, &workspace)?;
    Ok(workspace)
}

#[tauri::command]
pub async fn coding_isolation_create(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<IsolatedWorkspace, String> {
    let root = access.require_workspace(&root)?;
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
    let workspace = info(&root).ok_or_else(|| "当前项目不是隔离工作树".to_string())?;
    access.require_workspace(&workspace.source_root)?;
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
}
