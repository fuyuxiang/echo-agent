//! Installs and removes the OS watches that selection asks for.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::GlobSet;
use notify::RecursiveMode;
use notify_debouncer_full::{Debouncer, NoCache};
use tokio::sync::mpsc;

use crate::event::FsEventKind;
use crate::merge::RawFsEvent;
use crate::selection::{
    diff_watches, passes_custom_globs, pruning_walker, select_top_level_watch_dirs,
    skip_selection_below,
};

/// Re-runs [`select_top_level_watch_dirs`] so the running decision matches startup.
pub(crate) fn reconcile_top_level_watches(
    debouncer: &mut Debouncer<notify::RecommendedWatcher, NoCache>,
    watched: &mut HashSet<PathBuf>,
    root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
) {
    let desired: HashSet<PathBuf> =
        select_top_level_watch_dirs(root, custom_ignore, custom_include)
            .into_iter()
            .collect();

    let (to_add, to_remove) = diff_watches(&desired, watched);

    for dir in to_add {
        match debouncer.watch(&dir, RecursiveMode::Recursive) {
            Ok(()) => {
                watched.insert(dir);
            }
            Err(e) => tracing::warn!("failed to watch {dir:?}: {e:?}"),
        }
    }
    for dir in to_remove {
        let _ = debouncer.unwatch(&dir);
        watched.remove(&dir);
    }
}

/// Bounds the paths per synthetic event when a huge tree appears at once.
pub(crate) const BACKFILL_BATCH: usize = 512;

/// Watches armed per loop iteration. Each `watch()` is a 100 to 300µs round trip
/// into notify's thread, so this bounds command latency to about 0.1s a chunk
/// while a 50k directory backlog still arms in seconds.
pub(crate) const ARM_CHUNK: usize = 512;

/// Largest backlog armed before signaling ready, which keeps that wait under a
/// second while leaving a typical repository no startup blind window.
pub(crate) const ARM_SYNC_MAX: usize = 4096;

/// Arms up to [`ARM_CHUNK`] pending watches, tolerating entries that vanished
/// since selection.
pub(crate) fn arm_pending_chunk(
    debouncer: &mut Debouncer<notify::RecommendedWatcher, NoCache>,
    watched: &mut HashSet<PathBuf>,
    pending: &mut std::collections::VecDeque<PathBuf>,
    mode: RecursiveMode,
) {
    for _ in 0..ARM_CHUNK {
        let Some(dir) = pending.pop_front() else {
            break;
        };
        if watched.contains(&dir) {
            continue;
        }
        match debouncer.watch(&dir, mode) {
            Ok(()) => {
                watched.insert(dir);
            }
            Err(e) => tracing::debug!("failed to arm pending watch {dir:?}: {e:?}"),
        }
    }
    if pending.is_empty() {
        tracing::debug!(
            "fs_notify: watch arming complete ({} workspace dirs watched)",
            watched.len()
        );
    }
}

/// Per-dir mode: watch a newly created directory subtree.
pub(crate) fn add_subtree_watches(
    debouncer: &mut Debouncer<notify::RecommendedWatcher, NoCache>,
    watched: &mut HashSet<PathBuf>,
    watch_root: &Path,
    subtree_root: &Path,
    custom_ignore: &Option<GlobSet>,
    custom_include: &Option<GlobSet>,
    budget: usize,
    tx: &mpsc::UnboundedSender<RawFsEvent>,
) {
    // Symlink or vanished-since-event: nothing to watch.
    if !subtree_root
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_dir())
    {
        return;
    }
    if watched.contains(subtree_root) {
        return;
    }
    if skip_selection_below(watch_root, subtree_root) {
        tracing::debug!("fs_notify: not descending into {subtree_root:?}");
        return;
    }
    let mut backfill: Vec<PathBuf> = Vec::new();
    let flush = |paths: &mut Vec<PathBuf>| {
        if !paths.is_empty() {
            let _ = tx.send(RawFsEvent {
                paths: std::mem::take(paths),
                kind: FsEventKind::Created,
            });
        }
    };

    for entry in pruning_walker(subtree_root, custom_ignore, custom_include).flatten() {
        let path = entry.path();
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            if watched.contains(path) {
                continue;
            }
            if watched.len() >= budget {
                tracing::warn!(
                    "fs_notify: watch budget ({budget}) reached while adding {:?}; deeper dirs unwatched",
                    subtree_root
                );
                break;
            }
            match debouncer.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(path.to_path_buf());
                }
                Err(e) => tracing::warn!("failed to watch new dir {path:?}: {e:?}"),
            }
        } else if entry.depth() > 0 && passes_custom_globs(path, custom_ignore, custom_include) {
            backfill.push(path.to_path_buf());
            if backfill.len() >= BACKFILL_BATCH {
                flush(&mut backfill);
            }
        }
    }
    flush(&mut backfill);
}

/// Per-dir mode: drop bookkeeping (and best-effort OS watches) for a removed
/// or renamed-away directory subtree. The kernel already dropped watches on
/// deleted dirs (`IN_IGNORED`), but the explicit unwatch keeps notify's
/// path-keyed bookkeeping clean and — crucially for renames — frees the watch
/// descriptor *before* the destination path is re-watched.
pub(crate) fn prune_subtree_watches(
    debouncer: &mut Debouncer<notify::RecommendedWatcher, NoCache>,
    watched: &mut HashSet<PathBuf>,
    subtree_root: &Path,
) {
    let stale: Vec<PathBuf> = watched
        .iter()
        .filter(|p| p.starts_with(subtree_root))
        .cloned()
        .collect();
    for dir in stale {
        let _ = debouncer.unwatch(&dir);
        watched.remove(&dir);
    }
}
