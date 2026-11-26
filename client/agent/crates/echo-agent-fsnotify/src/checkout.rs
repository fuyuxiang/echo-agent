//! Recognizes a directory that roots a workspace other than the watched one.

use std::path::{Path, PathBuf};

use crate::handle::{WatchStrategy, watch_strategy};

pub(crate) fn is_another_workspace(dir: &Path) -> bool {
    let holds_a_checkout = dir.join(".git").symlink_metadata().is_ok() || dir.join(".sl").is_dir();
    holds_a_checkout && !is_declared_submodule(dir)
}

/// Whether a watcher rooted at `root` reaches `path`. Fan-out watches each
/// top-level child recursively, so only that decision excludes anything;
/// per-dir decides at every level.
pub fn watch_root_covers(root: &Path, path: &Path) -> bool {
    watch_root_covers_with(watch_strategy(), root, path)
}

/// [`watch_root_covers`] with the strategy passed in, so tests need not
/// mutate the process-global environment it is normally read from.
fn watch_root_covers_with(strategy: WatchStrategy, root: &Path, path: &Path) -> bool {
    let root = canonical(root);
    let path = canonical(path);
    let Ok(relative) = path.strip_prefix(&root) else {
        return false;
    };
    let decides_at_every_level = strategy == WatchStrategy::PerDir;
    let mut dir = root;
    for component in relative.components() {
        dir.push(component);
        if is_another_workspace(&dir) {
            return false;
        }
        if !decides_at_every_level {
            break;
        }
    }
    true
}

fn is_declared_submodule(dir: &Path) -> bool {
    let Some(parent) = dir.parent() else {
        return false;
    };
    let Ok(superproject) = git2::Repository::discover(parent) else {
        return false;
    };
    let Some(workdir) = superproject.workdir() else {
        return false;
    };
    let Ok(relative) = dir.strip_prefix(workdir) else {
        return false;
    };
    // `.gitmodules` records paths with forward slashes on every platform.
    let relative = relative.to_string_lossy().replace('\\', "/");
    superproject.find_submodule(&relative).is_ok()
}

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "checkout_tests.rs"]
mod tests;
