use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(super) const MAX_PROJECT_DIRS: usize = 16;
pub(super) const MAX_SANITIZED_LENGTH: usize = 200;

pub(super) fn scoped_project_dirs(config_dir: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut paths = vec![cwd.to_path_buf()];
    if let Ok(repository) = git2::Repository::discover(cwd) {
        if let Some(workdir) = repository.workdir() {
            paths.push(dunce::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf()));
            if repository.path() != repository.commondir()
                && let Some(main_workdir) = repository.commondir().parent()
            {
                paths.push(
                    dunce::canonicalize(main_workdir)
                        .unwrap_or_else(|_| main_workdir.to_path_buf()),
                );
            }
        }
        if let Ok(worktrees) = repository.worktrees() {
            for name in worktrees
                .iter()
                .filter_map(|name| name.ok().flatten())
                .take(MAX_PROJECT_DIRS)
            {
                if let Ok(worktree) = repository.find_worktree(name) {
                    let path = worktree.path();
                    paths.push(dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
                }
            }
        }
    }
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .take(MAX_PROJECT_DIRS)
        .filter_map(|path| project_dir_for_path(config_dir, &path))
        .collect()
}

pub(super) fn project_dir_for_path(config_dir: &Path, path: &Path) -> Option<PathBuf> {
    let project_dir = project_dir_path(config_dir, path)?;
    let metadata = std::fs::symlink_metadata(&project_dir).ok()?;
    (metadata.is_dir() && !metadata.file_type().is_symlink()).then_some(project_dir)
}

pub(super) fn project_dir_path(config_dir: &Path, path: &Path) -> Option<PathBuf> {
    let sanitized = path
        .to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    if sanitized.len() > MAX_SANITIZED_LENGTH {
        return None;
    }
    Some(config_dir.join("projects").join(sanitized))
}
