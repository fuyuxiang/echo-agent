//! Finds the repository governing a workspace and decides which of its
//! metadata files the watcher acts upon.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::RecursiveMode;

/// Permissive on purpose: lets `.git/index.lock` and `.git/gc.pid` through
/// to drive the lock state machine. `crate::paths::classify_git_path` keeps
/// them out of `GitMetaChanged`. Do not unify.
pub(crate) fn is_git_path_for_watcher(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains(".git/index")
        || s.contains(".git/HEAD")
        || s.contains(".git/FETCH_HEAD")
        || s.contains(".git/refs/")
        || s.contains(".git/packed-refs")
        || s.contains(".git/gc.pid")
}

/// Sapling analogue of [`is_git_path_for_watcher`]: lets **only** `.sl/wlock`
/// through. `.sl/dirstate` is intentionally not watched — it is read on demand,
/// because a read-only `sl status` rewrites dirstate without moving the parent,
/// so watching it would turn every status into a refresh storm. Forward-slash
/// only, like its git sibling.
pub(crate) fn is_sl_path_for_watcher(path: &Path) -> bool {
    path.to_string_lossy().contains(".sl/wlock")
}

/// True if `p`'s final component is exactly `name` (`.git`/`.sl`). Uses
/// `file_name` rather than `Path::ends_with` to dodge clippy's
/// `path_ends_with_ext` false positive on `.sl`.
pub(crate) fn dir_named(p: &Path, name: &str) -> bool {
    p.file_name().is_some_and(|n| n == name)
}

/// Whether Sapling (`.sl`) support is enabled (default on; `ECHO_AGENT_FSNOTIFY_SAPLING=0`
/// or `false` disables it). Resolved once per watcher in `FsEventSource::start_on`
/// and threaded down, so discovery, watching, and filtering can't disagree.
pub(crate) fn sapling_enabled() -> bool {
    !matches!(
        std::env::var("ECHO_AGENT_FSNOTIFY_SAPLING").ok().as_deref(),
        Some("0") | Some("false")
    )
}

#[derive(Default)]
pub(crate) struct GitignoreCache {
    cache: HashMap<PathBuf, (SystemTime, Gitignore)>,
}

impl GitignoreCache {
    /// Check if a path should be ignored.
    ///
    /// With `watch_vcs`, the metadata files that drive the lock state machine
    /// pass through: git's (`.git/index`, `.git/HEAD`, …) and, when `sapling`,
    /// `.sl/wlock`. Everything else under `.git`/`.sl` stays ignored.
    pub(crate) fn is_ignored(&mut self, path: &Path, watch_vcs: bool, sapling: bool) -> bool {
        let is_dir = path.is_dir();
        let mut current_dir = path.parent();
        while let Some(dir) = current_dir {
            if dir_named(dir, ".git") {
                if watch_vcs && is_git_path_for_watcher(path) {
                    return false;
                }
                return true;
            }
            if sapling && dir_named(dir, ".sl") {
                if watch_vcs && is_sl_path_for_watcher(path) {
                    return false;
                }
                return true;
            }

            let gitignore_path = dir.join(".gitignore");
            if let Ok(metadata) = gitignore_path.metadata()
                && let Ok(mtime) = metadata.modified()
            {
                let gitignore = self.get_or_load(&gitignore_path, dir, mtime);
                let m = gitignore.matched_path_or_any_parents(path, is_dir);
                if m.is_ignore() {
                    return true;
                }
                if m.is_whitelist() {
                    // A negation rule in this (deeper) .gitignore explicitly
                    // un-ignores the path. Shallower .gitignore files must not override.
                    return false;
                }
            }
            current_dir = dir.parent();
        }
        false
    }

    fn get_or_load(&mut self, gitignore_path: &Path, root: &Path, mtime: SystemTime) -> &Gitignore {
        let key = gitignore_path.to_path_buf();

        if let Some((cached_mtime, _)) = self.cache.get(&key)
            && *cached_mtime == mtime
        {
            return &self.cache[&key].1;
        }

        let mut builder = GitignoreBuilder::new(root);
        let _ = builder.add(gitignore_path);
        let gitignore = builder.build().unwrap_or_else(|_| Gitignore::empty());
        self.cache.insert(key.clone(), (mtime, gitignore));
        &self.cache[&key].1
    }
}

/// Locate the `.git` directory governing `watch_path` (searching its ancestors).
///
/// A real (non-symlink) `.git` directory is returned directly via a cheap
/// `symlink_metadata` check (no link-follow), and lives inside the canonical
/// ancestor so it can't escape. A `.git` file or symlink is resolved through
/// `git2`, which rejects a pointer to a non-git target (e.g. a planted
/// `gitdir: ~/.ssh` or `ln -s ~/.ssh .git`) instead of watching it.
pub(crate) fn find_git_dir(watch_path: &Path) -> Option<PathBuf> {
    for ancestor in watch_path.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = dot_git.symlink_metadata() else {
            continue;
        };
        if meta.file_type().is_dir() {
            return Some(dunce::canonicalize(&dot_git).unwrap_or(dot_git));
        }
        // A `.git` file or symlink: let git validate the target before watching.
        if let Ok(repo) = git2::Repository::open(ancestor) {
            let gd = repo.path().to_path_buf();
            return Some(dunce::canonicalize(&gd).unwrap_or(gd));
        }
    }
    None
}

/// Locate the `.sl` working-copy directory governing `watch_path` (ancestor
/// walk), mirroring [`find_git_dir`]'s real-directory branch: a non-symlink
/// `.sl` dir via `symlink_metadata` (no link-follow), canonicalized so it can't
/// escape. Sapling has no `.sl`-file indirection.
pub(crate) fn find_sl_dir(watch_path: &Path) -> Option<PathBuf> {
    for ancestor in watch_path.ancestors() {
        let dot_sl = ancestor.join(".sl");
        let Ok(meta) = dot_sl.symlink_metadata() else {
            continue;
        };
        if meta.file_type().is_dir() {
            return Some(dunce::canonicalize(&dot_sl).unwrap_or(dot_sl));
        }
    }
    None
}

/// Whether a discovered VCS metadata dir (`.git`/`.sl`) needs its own watch:
/// always when the root is non-recursive; under a recursive root only for an
/// *external* (ancestor) dir — an internal one is already covered.
pub(crate) fn should_watch_separate_vcs_dir(
    root_non_recursive: bool,
    vcs_dir: &Path,
    watch_path: &Path,
) -> bool {
    root_non_recursive || !vcs_dir.starts_with(watch_path)
}

/// The watches a discovered `.git` dir needs in per-dir mode, replacing the
/// fan-out mode's single recursive watch. Recursive `.git` is catastrophic on
/// inotify — `objects/` (256-way fan-out) and `modules/` (submodule git dirs)
/// are thousands of directories that the event filter would discard anyway.
///
/// Everything [`is_git_path_for_watcher`] passes is covered: `index`, `HEAD`,
/// `FETCH_HEAD`, `packed-refs`, `gc.pid` are direct children (non-recursive
/// `.git` watch); `refs/heads/**` + `refs/tags/**` recursive for branch/tag
/// moves. `refs` itself is non-recursive: `refs/remotes/**` (thousands of dirs
/// on fetch-heavy clones) is deliberately unwatched — remote updates still
/// surface via `FETCH_HEAD` and `packed-refs`. Worktree git dirs
/// (`.git/worktrees/<n>`) have no `refs/`, so they get just the non-recursive
/// watch, covering their `HEAD`/`index`.
pub(crate) fn per_dir_git_watches(git_dir: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let mut watches = vec![(git_dir.to_path_buf(), RecursiveMode::NonRecursive)];
    let refs = git_dir.join("refs");
    if refs.is_dir() {
        watches.push((refs.clone(), RecursiveMode::NonRecursive));
        for sub in ["heads", "tags"] {
            let p = refs.join(sub);
            if p.is_dir() {
                watches.push((p, RecursiveMode::Recursive));
            }
        }
    }
    watches
}
