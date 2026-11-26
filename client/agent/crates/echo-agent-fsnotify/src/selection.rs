//! Chooses the directories a watcher arms, and turns events into changes to
//! that choice.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

use crate::checkout::is_another_workspace;
use crate::event::FsEventKind;
use crate::vcs::dir_named;

pub(crate) fn build_globsets(patterns: &[String]) -> (Option<GlobSet>, Option<GlobSet>) {
    let mut ignore_builder = GlobSetBuilder::new();
    let mut include_builder = GlobSetBuilder::new();

    for pattern in patterns {
        let (is_negation, raw_pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |p| (true, p));

        let glob_pattern = if raw_pattern.starts_with("**/") || raw_pattern.starts_with('/') {
            raw_pattern.to_string()
        } else {
            format!("**/{raw_pattern}")
        };

        if let Ok(glob) = Glob::new(&glob_pattern) {
            if is_negation {
                include_builder.add(glob);
            } else {
                ignore_builder.add(glob);
            }
        } else {
            tracing::warn!("invalid pattern: {pattern}");
        }
    }

    let ignore_set = ignore_builder.build().ok().filter(|s| !s.is_empty());
    let include_set = include_builder.build().ok().filter(|s| !s.is_empty());

    (ignore_set, include_set)
}

/// Above this many top-level children, one recursive root watch costs less.
pub(crate) const MAX_TOP_LEVEL_FANOUT: usize = 64;

pub(crate) fn is_top_level_child(path: &Path, root: &Path) -> bool {
    path.parent() == Some(root)
}

/// True for the only events that can add or remove a top-level watch.
pub(crate) fn event_triggers_reconcile(kind: FsEventKind, paths: &[PathBuf], root: &Path) -> bool {
    matches!(
        kind,
        FsEventKind::Created | FsEventKind::Removed | FsEventKind::Renamed
    ) && paths.iter().any(|p| is_top_level_child(p, root))
}

/// Per-dir mode: classify one event's paths into watch-set delta candidates.
///
/// A structural event on an existing dir is both pruned and added, so a
/// delete+recreate inside one debounce window re-arms. A `.git`/`.sl` marker
/// whose parent is another workspace also prunes that parent: `git worktree
/// add` writes the marker last, so a directory can become another workspace
/// after selection accepted it.
pub(crate) fn scan_per_dir_updates(
    kind: FsEventKind,
    paths: &[PathBuf],
    pruned: &mut Vec<PathBuf>,
    added: &mut Vec<PathBuf>,
) {
    let structural = matches!(
        kind,
        FsEventKind::Created | FsEventKind::Removed | FsEventKind::Renamed
    );
    for p in paths {
        let is_dir = p.symlink_metadata().is_ok_and(|m| m.file_type().is_dir());
        if is_dir {
            if structural {
                pruned.push(p.clone());
            }
            added.push(p.clone());
        } else {
            pruned.push(p.clone());
        }
        if (dir_named(p, ".git") || dir_named(p, ".sl"))
            && let Some(parent) = p.parent()
            && skip_selection(parent)
        {
            pruned.push(parent.to_path_buf());
        }
    }
}

/// Apply the custom ignore/include globsets (a negation `include` wins).
pub(crate) fn passes_custom_globs(
    path: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
) -> bool {
    if let Some(include_set) = custom_include.as_ref()
        && include_set.is_match(path)
    {
        return true;
    }
    if let Some(ignore_set) = custom_ignore.as_ref()
        && ignore_set.is_match(path)
    {
        return false;
    }
    true
}

/// What selection refuses to descend into: VCS metadata, watched surgically
/// instead, and checkouts owned by another workspace. Never asked about the
/// workspace root, which holds the `.git` that would make it a checkout.
pub(crate) fn skip_selection(dir: &Path) -> bool {
    dir_named(dir, ".git") || dir_named(dir, ".sl") || is_another_workspace(dir)
}

/// [`skip_selection`] applied to `dir` and to every directory between it and
/// `root`. `git worktree add` writes its marker last, so a directory can be
/// queued as an add before the checkout above it is recognizable.
pub(crate) fn skip_selection_below(root: &Path, dir: &Path) -> bool {
    dir.ancestors()
        .take_while(|d| *d != root && d.starts_with(root))
        .any(skip_selection)
}

/// `WalkBuilder` honoring every ignore file, and never following symlinks so
/// watches cannot leave the workspace.
pub(crate) fn ignore_walker(root: &Path, max_depth: Option<usize>) -> ignore::Walk {
    configured_walker(root).max_depth(max_depth).build()
}

/// Every ignore source the watcher honors, and no symlink following, so a
/// watch cannot leave the workspace.
fn configured_walker(root: &Path) -> WalkBuilder {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .follow_links(false);
    walker
}

pub(crate) fn select_top_level_watch_dirs(
    root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
) -> Vec<PathBuf> {
    select_top_level_watch_dirs_capped(root, custom_ignore, custom_include, usize::MAX)
        .unwrap_or_default()
}

/// [`select_top_level_watch_dirs`], abandoned once the count exceeds `max`.
pub(crate) fn select_top_level_watch_dirs_capped(
    root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
    max: usize,
) -> Option<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in ignore_walker(root, Some(1)).flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        // Fan-out arms a recursive child watch, so there is no deeper prune.
        if skip_selection(path) {
            continue;
        }
        if passes_custom_globs(path, custom_ignore, custom_include) {
            if dirs.len() == max {
                return None;
            }
            dirs.push(path.to_path_buf());
        }
    }
    Some(dirs)
}

/// [`ignore_walker`] that also prunes descent into anything [`skip_selection`]
/// refuses.
pub(crate) fn pruning_walker(
    root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
) -> ignore::Walk {
    let mut walker = configured_walker(root);
    let custom_ignore = custom_ignore.clone();
    let custom_include = custom_include.clone();
    walker.filter_entry(move |entry| {
        if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
            return true;
        }
        let path = entry.path();
        if skip_selection(path) {
            return false;
        }
        passes_custom_globs(path, &custom_ignore, &custom_include)
    });
    walker.build()
}

pub(crate) fn select_per_dir_watch_dirs(
    root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = pruning_walker(root, custom_ignore, custom_include)
        .flatten()
        .filter(|e| e.depth() > 0 && e.file_type().is_some_and(|ft| ft.is_dir()))
        .map(|e| e.into_path())
        .collect();
    dirs.sort_by_key(|d| d.components().count());
    dirs
}

/// The directories to add and to remove, in that order.
pub(crate) fn diff_watches(
    desired: &HashSet<PathBuf>,
    live: &HashSet<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let to_add = desired.difference(live).cloned().collect();
    let to_remove = live.difference(desired).cloned().collect();
    (to_add, to_remove)
}
