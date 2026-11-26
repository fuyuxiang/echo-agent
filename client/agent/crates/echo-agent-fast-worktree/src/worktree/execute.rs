//! Worktree plan execution.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::copy::CopyStats;
use crate::copy::{self, ParallelCopyConfig};
use crate::git;
use crate::worktree::CreateWorktreeResult;
use crate::worktree::plan::WorktreePlan;
use crate::{IgnoredFilesMode, WorkingTreeMode};

/// Best-effort teardown of a partially built worktree at `dest`: removes the
/// directory and, for a linked worktree, its `.git/worktrees/<name>`
/// registration (`remove_worktree` reads the `gitdir:` pointer before deleting).
/// Used on cancel/error paths so a later pinned-dest fast path can't adopt a
/// half-built tree and git keeps no dangling worktree registration.
fn reclaim_partial_worktree(dest: &Path) {
    let _ = crate::remove_worktree(dest);
}

/// Removes a partially built worktree on drop unless [`disarm`](Self::disarm)ed,
/// so any early return (`?`/cancel) from a creation path tears down the partial
/// `dest`. Only arm this once no background thread is still writing into `dest`.
struct PartialWorktreeGuard<'a> {
    dest: &'a Path,
    armed: bool,
}

impl<'a> PartialWorktreeGuard<'a> {
    fn new(dest: &'a Path) -> Self {
        Self { dest, armed: true }
    }

    /// Consume the guard without reclaiming — the worktree completed successfully.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PartialWorktreeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            reclaim_partial_worktree(self.dest);
        }
    }
}

/// Join the standalone `.git/` copy thread, flattening a thread panic and the
/// inner copy error into a single `anyhow::Error`.
fn join_git_copy(
    handle: std::thread::JoinHandle<Result<crate::copy::gitdir::GitDirCopyStats>>,
) -> Result<crate::copy::gitdir::GitDirCopyStats> {
    handle
        .join()
        .map_err(|e| {
            anyhow::anyhow!(
                ".git/ copy thread panicked: {}",
                e.downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .or_else(|| e.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic")
            )
        })?
        .context("failed to copy .git/ directory")
}

/// Lock files (`*.lock`) and transient git state to remove from snapshots.
///
/// BTRFS and overlay snapshots capture the source tree atomically, including
/// stale lock files and in-progress operation state that would be poisonous
/// in the new worktree (e.g., a stale `index.lock` blocks every git operation).
///
/// This mirrors the skip list in `copy::gitdir::SKIP_TOP_LEVEL` +
/// `copy::gitdir::should_skip()` so that snapshot-based worktrees get the
/// same sanitized `.git/` state as standalone copy-based worktrees.
#[cfg(any(target_os = "linux", test))]
const SNAPSHOT_GIT_CLEANUP_TOP_LEVEL: &[&str] = &[
    // Linked worktree registrations — stale in a snapshot (point to source paths)
    "worktrees",
    // Transient HEAD-like state files — stale in a snapshot
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "AUTO_MERGE",
    "BISECT_LOG",
    // In-progress multi-step operation state
    "sequencer",
    "rebase-merge",
    "rebase-apply",
    // GC state
    "gc.log",
];

/// Remove lock files and transient git state from a snapshot worktree.
///
/// Called immediately after `btrfs subvolume snapshot` or overlay snapshot
/// creation, **before** any git operations (`git reset`, `git clean`, etc.)
/// because even `git reset --hard` will fail if `index.lock` exists.
///
/// Removes:
/// - All `*.lock` files in the `.git/` directory tree (at any depth)
/// - Transient state files (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, etc.)
/// - In-progress operation directories (`sequencer/`, `rebase-merge/`, `rebase-apply/`)
///
/// Returns the count of entries removed (for logging).
#[cfg(any(target_os = "linux", test))]
pub fn cleanup_snapshot_git_state(worktree_path: &Path) -> u32 {
    let git_dir = worktree_path.join(".git");
    if !git_dir.is_dir() {
        // Linked worktree (.git is a file) — nothing to clean up,
        // lock files live in the source repo's .git/worktrees/<name>/.
        return 0;
    }

    let mut removed = 0u32;

    // 1. Remove top-level transient state files and directories.
    for name in SNAPSHOT_GIT_CLEANUP_TOP_LEVEL {
        let path = git_dir.join(name);
        if path.is_dir() && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            tracing::debug!(path = %path.display(), "removed transient git state directory from snapshot");
        } else if path.is_file() && std::fs::remove_file(&path).is_ok() {
            removed += 1;
            tracing::debug!(path = %path.display(), "removed transient git state file from snapshot");
        }
    }

    // 2. Remove all *.lock files at any depth (excluding objects/ which has no locks).
    if let Ok(walker) = walk_for_lock_files(&git_dir) {
        for entry in walker {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lock")
                && path.is_file()
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
                tracing::debug!(path = %path.display(), "removed lock file from snapshot");
            }
        }
    }

    if removed > 0 {
        tracing::info!(
            worktree = %worktree_path.display(),
            removed,
            "cleaned up transient git state from snapshot"
        );
    }

    removed
}

/// Walk `.git/` for lock files, skipping `objects/` (which is large and has no locks).
#[cfg(any(target_os = "linux", test))]
fn walk_for_lock_files(
    git_dir: &Path,
) -> Result<impl Iterator<Item = Result<std::fs::DirEntry, std::io::Error>>> {
    Ok(walkdir_skip_objects(git_dir).into_iter())
}

/// Simple recursive directory iterator that skips `objects/` subdirectory.
#[cfg(any(target_os = "linux", test))]
fn walkdir_skip_objects(root: &Path) -> Vec<Result<std::fs::DirEntry, std::io::Error>> {
    let mut results = Vec::new();
    walkdir_recurse(root, root, &mut results);
    results
}

#[cfg(any(target_os = "linux", test))]
fn walkdir_recurse(
    root: &Path,
    dir: &Path,
    results: &mut Vec<Result<std::fs::DirEntry, std::io::Error>>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            results.push(Err(e));
            return;
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            results.push(entry);
            continue;
        };

        // Skip objects/ at the top level — it's huge and never has lock files.
        if dir == root && entry.file_name() == "objects" {
            continue;
        }

        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_dir() {
            walkdir_recurse(root, &entry.path(), results);
        } else {
            results.push(Ok(entry));
        }
    }
}

/// Execute worktree creation. This is a blocking operation.
pub(crate) fn execute_create_worktree(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    let source = plan.source.clone();
    let start = std::time::Instant::now();
    let result = execute_create_worktree_dispatch(plan)?;
    crate::metrics::record_grove_wt_create(result.resolved_strategy, start.elapsed());
    record_main_repo_marker(&source, &result.worktree_path);
    Ok(result)
}

/// Record the source repo root in `<worktree>/.git/echo-agent-worktree-source`.
///
/// A standalone worktree is an independent repo whose `.git` is a directory:
/// nothing inside it points back to the source, so consumers like `.envrc`
/// cannot recover the shared repo (e.g. to set a shared `CARGO_TARGET_DIR`).
/// Linked worktrees (`.git` is a file) resolve it via
/// `git rev-parse --git-common-dir` and are skipped. An existing marker is
/// left intact: it was inherited from a worktree source and already points
/// at the ultimate main repo.
fn record_main_repo_marker(source: &Path, worktree: &Path) {
    let git_dir = worktree.join(".git");
    if !git_dir.is_dir() {
        return;
    }
    let marker = git_dir.join("echo-agent-worktree-source");
    if marker.exists() {
        return;
    }
    let main_repo = match git::find_worktree_root(source) {
        Ok(root) => root,
        Err(e) => {
            tracing::warn!(error = %e, source = %source.display(), "worktree source marker: cannot resolve main repo root");
            return;
        }
    };
    if let Err(e) = std::fs::write(&marker, main_repo.to_string_lossy().as_bytes()) {
        tracing::warn!(error = %e, marker = %marker.display(), "failed to write worktree source marker");
    }
}

/// Dispatch worktree creation to the strategy implied by the creation mode.
fn execute_create_worktree_dispatch(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    use crate::CreationMode;

    match &plan.creation_mode {
        CreationMode::Linked | CreationMode::Standalone => {
            // Track why fast paths were skipped so the copy fallback error
            // (if any) includes context about what was tried first.
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let mut skipped_reasons: Vec<String> = Vec::new();

            // macOS: grove-nfs first. Linux: overlay → btrfs → grove-fuse.
            #[cfg(target_os = "macos")]
            {
                match crate::nfs::try_grove_worktree(&plan) {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => {}
                    Err(e) if crate::nfs::nfs_error_blocks_fallback(&e) => return Err(e),
                    Err(e) => {
                        tracing::warn!(error = %e, "grove-nfs worktree failed, falling back to copy");
                        skipped_reasons.push(format!("grove-nfs: {e:#}"));
                    }
                }
            }

            // 1. Try overlay-on-FUSE snapshot (O(1), no file copies)
            #[cfg(target_os = "linux")]
            {
                match try_overlay_worktree(&plan) {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "overlay snapshot failed, falling back to next strategy"
                        );
                        skipped_reasons.push(format!("overlay: {e:#}"));
                    }
                }
            }

            // 2. Try BTRFS snapshot (O(1), no file copies)
            #[cfg(target_os = "linux")]
            {
                match try_btrfs_worktree(&plan) {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "btrfs snapshot failed, falling back to file copy"
                        );
                        skipped_reasons.push(format!("btrfs: {e:#}"));
                    }
                }
            }

            #[cfg(target_os = "linux")]
            {
                match crate::nfs::try_grove_worktree(&plan) {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => {}
                    Err(e) if crate::nfs::nfs_error_blocks_fallback(&e) => return Err(e),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "grove-fuse worktree failed, falling back to copy"
                        );
                        skipped_reasons.push(format!("grove-fuse: {e:#}"));
                    }
                }
            }

            // 3. Fall back to file-by-file copy
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if !skipped_reasons.is_empty() {
                tracing::info!(
                    reasons = skipped_reasons.join("; "),
                    "using file copy fallback (fast paths failed)"
                );
            }

            match &plan.creation_mode {
                CreationMode::Linked => execute_copy_worktree(plan),
                CreationMode::Standalone => execute_standalone_worktree(plan),
                _ => unreachable!(),
            }
        }
        CreationMode::GitCheckout => execute_git_checkout_worktree(plan),
    }
}

/// Whether the overlay strategy must be skipped, given the mount-namespace
/// classification.
///
/// An overlayfs mount is namespace-local and (unlike a btrfs snapshot) cannot be
/// exposed via a symlink, so it must not be used inside a private mount
/// namespace. Skip only for a positively-determined `Private` namespace;
/// `Host` and `Unknown` keep overlay enabled (see
/// `mount_info::current_mount_ns_status` for the `Unknown→false` rationale).
/// Pure function so the call-site polarity is pinned in one unit-tested place.
#[cfg(target_os = "linux")]
fn should_skip_overlay(status: crate::mount_info::MountNsStatus) -> bool {
    matches!(status, crate::mount_info::MountNsStatus::Private)
}

/// Whether an overlay snapshot's `upper` holds any working-tree change.
///
/// overlayfs copy-up is path-based, so a top-level `readdir` showing only `.git`
/// proves nothing is modified or untracked — a ~instant local check vs the ~4s
/// FUSE walk `git clean` would do. Returns `true` on read error (run clean).
#[cfg(target_os = "linux")]
fn overlay_upper_has_worktree_changes(upper: &Path) -> bool {
    match std::fs::read_dir(upper) {
        Ok(entries) => entries
            .flatten()
            .any(|e| e.file_name() != std::ffi::OsStr::new(".git")),
        Err(_) => true,
    }
}

/// Apply the working-tree mode + checkout on a freshly snapshotted worktree,
/// skipping git steps already satisfied. Shared by the overlay/btrfs/delegate
/// paths. On repo-fuse each git step stats through FUSE (~5s cold per full-tree
/// walk), so we skip aggressively: `rev-parse` skips `checkout` when HEAD is
/// already at the ref, and a clean `overlay_upper` lets us skip `reset`/`clean`
/// and their walk guards. Non-overlay paths pass `None` (local btrfs — cheap).
/// Returns the worktree HEAD commit.
#[cfg(target_os = "linux")]
fn finalize_clean_and_ref(
    worktree_path: &Path,
    working_tree: &WorkingTreeMode,
    git_ref: &str,
    overlay_upper: Option<&Path>,
) -> Result<String> {
    // Fast path: an overlay snapshot is provably clean iff its upper has no
    // copy-ups/untracked files (readdir shows only `.git`) AND the index has no
    // staged changes (`diff-index --cached`). Both checks are cheap and avoid the
    // ~5s cold FUSE walk that `reset`/`clean` and their `diff-index` guard do.
    let overlay_pristine = match (working_tree, overlay_upper) {
        (WorkingTreeMode::CleanTracked | WorkingTreeMode::CleanAll, Some(upper)) => {
            !overlay_upper_has_worktree_changes(upper) && !git::has_staged_changes(worktree_path)?
        }
        _ => false,
    };

    match working_tree {
        WorkingTreeMode::PreserveWorkingTree => {}
        _ if overlay_pristine => {
            tracing::debug!("skipping git reset/clean: overlay snapshot is clean");
        }
        WorkingTreeMode::CleanTracked => {
            if git::worktree_has_tracked_changes(worktree_path)? {
                git::git_reset_hard_command(worktree_path, None)?;
            } else {
                tracing::debug!("skipping git reset --hard: snapshot already clean");
            }
        }
        WorkingTreeMode::CleanAll => {
            if git::worktree_has_tracked_changes(worktree_path)? {
                git::git_reset_hard_command(worktree_path, None)?;
            } else {
                tracing::debug!("skipping git reset --hard: snapshot already clean");
            }
            git::git_clean_fd(worktree_path, false)?;
        }
    }

    if git_ref != "HEAD" {
        if git::worktree_at_ref(worktree_path, git_ref)? {
            tracing::debug!(git_ref, "skipping git checkout: HEAD already at ref");
        } else {
            git::checkout_ref(worktree_path, git_ref)?;
        }
    }

    let dest_git = worktree_path.join(".git");
    let keep = copy::standalone::origin_keep_names_for_git_ref(&dest_git, git_ref);
    copy::standalone::sanitize_standalone_git_dir_keeping(&dest_git, &keep)
        .context("failed to sanitize standalone .git/ after snapshot")?;

    git::get_head_commit(worktree_path).context("failed to get HEAD commit")
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static INJECT_OVERLAY_SOME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INJECT_BTRFS_SOME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn set_inject_overlay_some(v: bool) {
    INJECT_OVERLAY_SOME.with(|c| c.set(v));
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn set_inject_btrfs_some(v: bool) {
    INJECT_BTRFS_SOME.with(|c| c.set(v));
}

#[cfg(all(test, target_os = "linux"))]
fn dummy_injected_result(plan: &WorktreePlan, strategy: &'static str) -> CreateWorktreeResult {
    CreateWorktreeResult {
        worktree_path: plan.dest.clone(),
        commit: "0".repeat(40),
        copy_stats: CopyStats::default(),
        ignored_stats: None,
        dirty_files_report: None,
        resolved_strategy: strategy,
        strategy_metadata: None,
    }
}

/// Try to create worktree using overlay-on-FUSE snapshot.
/// Returns `Ok(Some(result))` if overlay was used, `Ok(None)` to fall back.
#[cfg(target_os = "linux")]
fn try_overlay_worktree(plan: &WorktreePlan) -> Result<Option<CreateWorktreeResult>> {
    #[cfg(test)]
    if INJECT_OVERLAY_SOME.with(std::cell::Cell::get) {
        return Ok(Some(dummy_injected_result(
            plan,
            crate::worktree::STRATEGY_OVERLAY,
        )));
    }
    use crate::overlay;

    // Skip the namespace-local overlay mount in a private mount namespace (see
    // `should_skip_overlay`) — unless a delegate can mount inside our namespace
    // for us, in which case the overlay is reachable even when sandboxed.
    let has_delegate = plan.btrfs_delegate.is_some();
    if !has_delegate && should_skip_overlay(crate::mount_info::current_mount_ns_status()) {
        tracing::info!(
            "private mount namespace detected, skipping overlay snapshot strategy \
             (namespace-local mounts vanish on restart and across shells)"
        );
        return Ok(None);
    }

    let source_root = git::find_worktree_root(&plan.source)
        .with_context(|| format!("failed to find git root for {}", plan.source.display()))?;

    let info = match overlay::detect_fuse_overlay(&source_root)? {
        Some(info) => info,
        None => return Ok(None),
    };

    tracing::info!(
        source = %source_root.display(),
        lower = %info.lower_dir.display(),
        upper = %info.upper_dir.display(),
        "detected FUSE+overlay with btrfs upper, using overlay snapshot for O(1) worktree"
    );

    execute_overlay_worktree(plan.clone(), info).map(Some)
}

/// Execute worktree creation using overlay-on-FUSE snapshot.
#[cfg(target_os = "linux")]
fn execute_overlay_worktree(
    plan: WorktreePlan,
    info: crate::overlay::OverlayInfo,
) -> Result<CreateWorktreeResult> {
    let start = std::time::Instant::now();

    // Ensure parent directory exists.
    if let Some(parent) = plan.dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for worktree dest: {}",
                parent.display()
            )
        })?;
    }

    // Snapshot upper + write metadata + mount overlay. Rootless callers pass a
    // delegate so a privileged helper performs the mount in our namespace.
    let snapshot_start = std::time::Instant::now();
    let result =
        crate::overlay::create_overlay_worktree(&info, &plan.dest, plan.btrfs_delegate.as_ref())?;
    tracing::info!(
        elapsed = ?snapshot_start.elapsed(),
        snapshot = %result.snapshot_root.display(),
        "overlay snapshot created and mounted"
    );

    // Post-mount git operations. If any fail, clean up the overlay mount
    // and btrfs snapshot so we don't leave stale resources behind.
    let snapshot_upper = result.snapshot_root.join("upper");
    let post_mount = || -> Result<String> {
        // Clean up stale lock files and transient git state from the snapshot.
        // This MUST happen before any git operations because even `git reset --hard`
        // will block indefinitely if the source had an `index.lock` at snapshot time.
        cleanup_snapshot_git_state(&plan.dest);

        // Pass the snapshot upper so the clean fast path can engage.
        finalize_clean_and_ref(
            &plan.dest,
            &plan.working_tree,
            &plan.git_ref,
            Some(&snapshot_upper),
        )
    };

    // Same reclaim-on-post-op-failure shape as the btrfs/delegate arms.
    let commit = finalize_or_reclaim_snapshot(post_mount, || {
        let _ = crate::overlay::remove_overlay_worktree(
            &plan.dest,
            &result.snapshot_root,
            &result.work_dir,
            plan.btrfs_delegate.as_ref(),
        );
    })?;

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        method = "overlay_snapshot",
        "worktree created via overlay snapshot"
    );

    Ok(CreateWorktreeResult {
        worktree_path: plan.dest,
        commit,
        copy_stats: CopyStats::default(), // 0 files copied!
        ignored_stats: None,              // overlay includes everything
        dirty_files_report: None,
        resolved_strategy: crate::worktree::STRATEGY_OVERLAY,
        strategy_metadata: Some(serde_json::json!({
            "overlay": { "snapshot_root": result.snapshot_root }
        })),
    })
}

/// Try to create worktree using BTRFS snapshot.
/// Returns Ok(Some(result)) if BTRFS was used, Ok(None) if we should fall back to copy.
#[cfg(target_os = "linux")]
fn try_btrfs_worktree(plan: &WorktreePlan) -> Result<Option<CreateWorktreeResult>> {
    #[cfg(test)]
    if INJECT_BTRFS_SOME.with(std::cell::Cell::get) {
        return Ok(Some(dummy_injected_result(
            plan,
            crate::worktree::STRATEGY_BTRFS,
        )));
    }
    use crate::btrfs;

    // Get source git root
    let source_root = git::find_worktree_root(&plan.source)
        .with_context(|| format!("failed to find git root for {}", plan.source.display()))?;

    // Auto-detect: check if source is a BTRFS subvolume
    let btrfs_info = match btrfs::is_btrfs_subvolume(&source_root)? {
        Some(info) => {
            // Verify git root matches subvolume root
            if source_root != info.subvolume_root {
                tracing::info!(
                    source_root = %source_root.display(),
                    subvolume_root = %info.subvolume_root.display(),
                    "git root differs from BTRFS subvolume root, falling back to copy"
                );
                return try_btrfs_delegate(plan);
            }

            // Log bind mount detection
            if let Some(ref bind_source) = info.bind_mount_source {
                tracing::info!(
                    source = %source_root.display(),
                    bind_source = %bind_source.display(),
                    btrfs_mount = ?info.btrfs_mount_point,
                    "detected bind-mounted BTRFS subvolume, using snapshot with bind mount"
                );
            } else {
                tracing::info!(
                    source = %source_root.display(),
                    "detected direct BTRFS subvolume, using snapshot"
                );
            }

            info
        }
        None => {
            tracing::debug!(
                source = %source_root.display(),
                "source is not a BTRFS subvolume, trying delegate"
            );
            return try_btrfs_delegate(plan);
        }
    };

    // Execute BTRFS snapshot worktree creation with bind mount support.
    // If the direct path fails (e.g., EPERM in sandbox), try the delegate.
    match execute_btrfs_worktree(plan.clone(), btrfs_info) {
        Ok(result) => Ok(Some(result)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "direct btrfs snapshot failed, trying delegate fallback"
            );
            try_btrfs_delegate(plan)
        }
    }
}

/// Run the post-snapshot git ops; on failure, run `reclaim` and propagate the
/// original error. Snapshot creation succeeds before the working-tree
/// reset/clean/checkout, so a later git failure must not leave the snapshot
/// behind. Pure combinator so the failure→cleanup branch is unit-testable
/// without root+btrfs (mirrors `btrfs::snapshot::expose_or_reclaim_snapshot`).
#[cfg(target_os = "linux")]
fn finalize_or_reclaim_snapshot(
    post: impl FnOnce() -> Result<String>,
    reclaim: impl FnOnce(),
) -> Result<String> {
    match post() {
        Ok(commit) => Ok(commit),
        Err(e) => {
            tracing::warn!(error = %e, "post-snapshot git operation failed, reclaiming snapshot");
            reclaim();
            Err(e)
        }
    }
}

/// Reclaim a just-created btrfs snapshot whose post-snapshot git ops failed:
/// delete the subvolume, drop its recovery metadata, and remove the exposing
/// symlink. Without this the symlink keeps the snapshot "active" so the orphan
/// scanner never reclaims it. Best-effort: a failed delete is logged, not fatal.
#[cfg(target_os = "linux")]
fn reclaim_btrfs_snapshot(snapshot: &crate::btrfs::snapshot::SnapshotResult) {
    use crate::btrfs;
    if let Err(e) = btrfs::delete_snapshot(&snapshot.snapshot_path) {
        tracing::warn!(
            error = %e,
            path = %snapshot.snapshot_path.display(),
            "failed to delete btrfs snapshot subvolume during reclaim"
        );
    }
    btrfs::remove_btrfs_metadata(&snapshot.snapshot_path);
    if let Some(symlink) = &snapshot.symlink_path {
        let _ = std::fs::remove_file(symlink);
    }
}

/// Try to create a BTRFS worktree via the delegate (privileged helper).
///
/// Returns `Ok(Some(result))` if the delegate succeeded, `Ok(None)` if no
/// delegate is configured (fall through to copy), or `Err` on hard failures.
#[cfg(target_os = "linux")]
fn try_btrfs_delegate(plan: &WorktreePlan) -> Result<Option<CreateWorktreeResult>> {
    let delegate = match &plan.btrfs_delegate {
        Some(d) => d,
        None => return Ok(None),
    };

    tracing::info!(
        source = %plan.source.display(),
        dest = %plan.dest.display(),
        "attempting btrfs worktree creation via delegate"
    );

    let start = std::time::Instant::now();

    let snapshot = match delegate.create_snapshot(&plan.source, &plan.dest) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "delegate btrfs snapshot failed, falling back to copy"
            );
            return Ok(None);
        }
    };

    tracing::info!(
        elapsed = ?start.elapsed(),
        snapshot_path = %snapshot.snapshot_path.display(),
        worktree_path = %snapshot.worktree_path.display(),
        bind_mounted = snapshot.bind_mounted,
        "delegate created btrfs snapshot"
    );

    // The delegate has already created the snapshot, bind mount, and cleaned up
    // git state. Now do the remaining git operations on the worktree. If any
    // fail, reclaim the delegate-created snapshot so it isn't leaked.
    let worktree_path = &snapshot.worktree_path;

    let post_snapshot = || -> Result<String> {
        // Local btrfs snapshot (no FUSE) — None keeps the cheap unconditional clean.
        finalize_clean_and_ref(worktree_path, &plan.working_tree, &plan.git_ref, None)
    };

    let commit = finalize_or_reclaim_snapshot(post_snapshot, || {
        if let Err(del_err) = delegate.delete_snapshot(&plan.dest) {
            tracing::warn!(error = %del_err, "failed to reclaim delegate btrfs snapshot");
        }
    })?;

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        method = "btrfs_delegate",
        "worktree created via btrfs delegate"
    );

    Ok(Some(CreateWorktreeResult {
        worktree_path: plan.dest.clone(),
        commit,
        copy_stats: CopyStats::default(),
        ignored_stats: None,
        dirty_files_report: None,
        resolved_strategy: crate::worktree::STRATEGY_BTRFS,
        strategy_metadata: Some(serde_json::json!({
            "btrfs": { "worktree_path": plan.dest }
        })),
    }))
}

/// Execute worktree creation using BTRFS snapshot.
#[cfg(target_os = "linux")]
fn execute_btrfs_worktree(
    plan: WorktreePlan,
    btrfs_info: crate::btrfs::BtrfsInfo,
) -> Result<CreateWorktreeResult> {
    use crate::btrfs;

    let source_root = git::find_worktree_root(&plan.source)
        .with_context(|| format!("failed to find git root for {}", plan.source.display()))?;

    let is_symlinked = btrfs_info.bind_mount_source.is_some();

    tracing::info!(
        source = %source_root.display(),
        dest = %plan.dest.display(),
        working_tree = ?plan.working_tree,
        is_symlinked = is_symlinked,
        "creating worktree via BTRFS snapshot"
    );

    let start = std::time::Instant::now();

    // Ensure parent directory exists
    if let Some(parent) = plan.dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for worktree dest: {}",
                parent.display()
            )
        })?;
    }

    // O(1) snapshot; bind-mounted sources are exposed at `dest` via a symlink.
    let snapshot_start = std::time::Instant::now();
    let snapshot_result = btrfs::create_snapshot_with_symlink(&btrfs_info, &plan.dest)?;
    tracing::info!(
        elapsed = ?snapshot_start.elapsed(),
        snapshot_path = %snapshot_result.snapshot_path.display(),
        symlinked = snapshot_result.symlink_path.is_some(),
        "BTRFS snapshot created"
    );

    // `dest` is the direct snapshot, or (bind-mounted source) a symlink to the
    // on-disk snapshot. Consumers that canonicalize the worktree cwd resolve
    // through the symlink; prefix checks stay consistent because they canonicalize
    // both sides. Matches the long-standing delegate (rootless host) behavior.
    let worktree_path = snapshot_result
        .symlink_path
        .as_ref()
        .unwrap_or(&snapshot_result.snapshot_path);

    // Post-snapshot git operations. If any fail, reclaim the snapshot
    // (subvolume + metadata + symlink) — the symlink would otherwise keep an
    // unusable worktree "active" forever (the orphan scanner skips active
    // symlinks). Mirrors the overlay path's post-mount cleanup.
    let post_snapshot = || -> Result<String> {
        // Clean up stale lock files and transient git state from the snapshot.
        // This MUST happen before any git operations because even `git reset --hard`
        // will block indefinitely if the source had an `index.lock` at snapshot time.
        cleanup_snapshot_git_state(worktree_path);

        // Local btrfs snapshot (no FUSE) — None keeps the cheap unconditional clean.
        finalize_clean_and_ref(worktree_path, &plan.working_tree, &plan.git_ref, None)
    };

    let commit =
        finalize_or_reclaim_snapshot(post_snapshot, || reclaim_btrfs_snapshot(&snapshot_result))?;

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        method = if is_symlinked { "btrfs_snapshot_with_symlink" } else { "btrfs_snapshot" },
        "worktree created via BTRFS snapshot"
    );

    Ok(CreateWorktreeResult {
        worktree_path: plan.dest,
        commit,
        copy_stats: CopyStats::default(), // No files copied - instant snapshot!
        ignored_stats: None,              // Snapshot includes everything
        dirty_files_report: None,         // Not tracked for BTRFS snapshots
        resolved_strategy: crate::worktree::STRATEGY_BTRFS,
        strategy_metadata: None,
    })
}

/// Execute worktree creation using file-by-file copy (original implementation).
fn execute_copy_worktree(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    let effective_parallelism = plan.effective_parallelism();
    let effective_ignored_parallelism = plan.effective_ignored_parallelism();

    let WorktreePlan {
        source,
        dest,
        git_ref,
        parallelism: _,
        channel_buffer,
        working_tree,
        ignored_files,
        ignored_parallelism: _,
        creation_mode: _,
        cancellation_token,
        btrfs_delegate: _,
        worktree_id: _,
        nfs: _,
    } = plan;

    // CRITICAL: Resolve the actual git worktree root from the source path.
    // The source might be a subdirectory (e.g., /repo/subdir), but we need to
    // copy from the git root (e.g., /repo) to ensure all files are included.
    let source_root = git::find_worktree_root(&source)
        .with_context(|| format!("failed to find git root for {}", source.display()))?;

    tracing::info!(
        source = %source.display(),
        source_root = %source_root.display(),
        dest = %dest.display(),
        parallelism = effective_parallelism,
        working_tree = ?working_tree,
        ignored_files = ?ignored_files,
        method = "copy",
        "creating fast worktree"
    );

    let start = std::time::Instant::now();

    // Phase 1: Create worktree with --no-checkout.
    // Ensure that the parent directory exists before creating the worktree.
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for worktree dest: {}",
                parent.display()
            )
        })?;
    }

    let dest_str = dest.to_string_lossy().to_string();

    // Get modified files from source (for dirty state preservation or clean modes).
    // This runs in parallel with worktree creation conceptually, but since we're sync now,
    // we run it after worktree add for simplicity (the worktree add is typically fast).
    git::worktree_add_no_checkout(&source, &dest_str, &git_ref)?;
    tracing::debug!(elapsed = ?start.elapsed(), "git worktree add --no-checkout complete");

    // `git worktree add` created dest + its `.git/worktrees/<name>` registration.
    // From here, any early return (cancel or a hard error in copy/finalize/
    // bad-ref) must reclaim both so a later pinned-dest fast path can't adopt a
    // partial tree. No background threads touch dest in this path, so a drop
    // guard is sufficient.
    let guard = PartialWorktreeGuard::new(&dest);

    // Check cancellation after git worktree add (before the expensive copy).
    if cancellation_token.is_cancelled() {
        anyhow::bail!("cancelled after git worktree add");
    }

    // Get modified files
    let modified_result = git::get_modified_files(&source_root)?;
    let modified_files_in_source = Arc::new(modified_result.paths);
    let dirty_files_report = Some(modified_result.report);

    // For CleanTracked/CleanAll: we need to skip modified files during copy
    let modified_files_for_skip = match &working_tree {
        WorkingTreeMode::PreserveWorkingTree => None,
        WorkingTreeMode::CleanTracked | WorkingTreeMode::CleanAll => {
            Some(Arc::clone(&modified_files_in_source))
        }
    };

    // Phase 2: Parallel CoW copy of unignored files.
    // IMPORTANT: Copy from source_root (git root), not source (which might be a subdirectory).
    let copy_start = std::time::Instant::now();
    let copy_config = ParallelCopyConfig {
        num_workers: effective_parallelism,
        channel_buffer,
        skip_files: modified_files_for_skip,
        respect_gitignore: true,
        skip_patterns: vec![],
    };

    let copy_result =
        copy::copy_parallel(&source_root, &dest, copy_config, cancellation_token.clone())?;

    tracing::debug!(
        elapsed = ?copy_start.elapsed(),
        files = copy_result.stats.files_copied,
        dirs = copy_result.stats.dirs_created,
        "parallel copy complete"
    );

    // Check cancellation after the heavy copy phase.
    if cancellation_token.is_cancelled() {
        anyhow::bail!("cancelled after parallel copy");
    }

    // Phase 3: Finalize (index copy / reset / clean).
    let finalize_start = std::time::Instant::now();

    finalize_worktree(
        &source_root,
        &dest,
        working_tree,
        &modified_files_in_source,
        copy_result.file_metadata,
    )?;
    tracing::debug!(elapsed = ?finalize_start.elapsed(), "finalize complete");

    // Phase 4: Copy ignored files (optional).
    let ignored_stats = match ignored_files {
        IgnoredFilesMode::Skip | IgnoredFilesMode::CopyOnly { .. } => None,
        IgnoredFilesMode::Copy { skip_patterns } => {
            let ignored_start = std::time::Instant::now();
            let already_copied = copy_result.copied_paths;

            let copy_config = ParallelCopyConfig {
                num_workers: effective_ignored_parallelism,
                channel_buffer,
                skip_files: Some(Arc::new(already_copied)),
                respect_gitignore: false, // We want all files.
                skip_patterns,
            };

            let stats = copy::copy_parallel(&source_root, &dest, copy_config, cancellation_token)?;

            tracing::debug!(
                elapsed = ?ignored_start.elapsed(),
                files = stats.stats.files_copied,
                skipped = stats.stats.files_skipped,
                "ignored files copy complete"
            );
            Some(stats.stats)
        }
    };

    // Get the commit using gix.
    let commit = git::get_head_commit(&dest).context("failed to get HEAD commit")?;

    // Worktree complete — don't reclaim.
    guard.disarm();

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        files_copied = copy_result.stats.files_copied,
        ignored_files_copied = ignored_stats.as_ref().map(|s| s.files_copied).unwrap_or(0),
        "fast worktree created"
    );

    Ok(CreateWorktreeResult {
        worktree_path: dest,
        commit,
        copy_stats: copy_result.stats,
        ignored_stats,
        dirty_files_report,
        resolved_strategy: crate::worktree::STRATEGY_COPY,
        strategy_metadata: None,
    })
}

fn finalize_worktree(
    source: &Path,
    dest: &Path,
    working_tree: WorkingTreeMode,
    modified_files_in_source: &Arc<dashmap::DashSet<std::path::PathBuf>>,
    file_metadata: dashmap::DashMap<std::path::PathBuf, std::fs::Metadata>,
) -> Result<()> {
    match working_tree {
        WorkingTreeMode::PreserveWorkingTree => {
            // For PreserveWorkingTree we need the source's index (which
            // reflects staged changes) and then update stat caches to
            // match the newly copied files. This is the only mode where
            // copying the index is correct — the files were CoW'd from
            // the source so we want the source's staging state.
            git::copy_git_index(source, dest)?;
            let clean = collect_clean_metadata(file_metadata, modified_files_in_source);
            git::update_index_stats(dest, &clean)?;
        }
        WorkingTreeMode::CleanTracked => {
            // `git reset --hard` rebuilds the index from HEAD with
            // correct stat caches. No need to copy the index first —
            // that would just be overwritten by reset.
            git::git_reset_hard_command(dest, None)?;
        }
        WorkingTreeMode::CleanAll => {
            // Same as CleanTracked but also remove untracked files.
            git::git_reset_hard_command(dest, None)?;
            git::git_clean_fd(dest, false)?;
        }
    }

    Ok(())
}

/// Filter file metadata to only include clean (unmodified) files.
fn collect_clean_metadata(
    file_metadata: dashmap::DashMap<std::path::PathBuf, std::fs::Metadata>,
    modified_files: &dashmap::DashSet<std::path::PathBuf>,
) -> Vec<(std::path::PathBuf, std::fs::Metadata)> {
    let mut clean = Vec::with_capacity(file_metadata.len());
    for entry in file_metadata.into_iter() {
        if !modified_files.contains(&entry.0) {
            clean.push(entry);
        }
    }
    clean
}

/// Execute worktree creation as a standalone repository copy.
///
/// Instead of using `git worktree add` (which creates a linked worktree sharing
/// the source's object store), this CoW's the `.git/` directory to create a
/// fully independent repository. The result can be promoted to replace the
/// source via a simple `rename()`.
///
/// For `PreserveWorkingTree` mode, the index stat update is fire-and-forget
/// (runs in a background thread) so the caller gets the result immediately.
fn execute_standalone_worktree(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    let effective_parallelism = plan.effective_parallelism();
    let effective_ignored_parallelism = plan.effective_ignored_parallelism();

    let WorktreePlan {
        source,
        dest,
        git_ref,
        parallelism: _,
        channel_buffer,
        working_tree,
        ignored_files,
        ignored_parallelism: _,
        creation_mode: _,
        cancellation_token,
        btrfs_delegate: _,
        worktree_id: _,
        nfs: _,
    } = plan;

    let source_root = git::find_worktree_root(&source)
        .with_context(|| format!("failed to find git root for {}", source.display()))?;

    tracing::info!(
        source = %source.display(),
        source_root = %source_root.display(),
        dest = %dest.display(),
        parallelism = effective_parallelism,
        working_tree = ?working_tree,
        ignored_files = ?ignored_files,
        method = "standalone",
        "creating standalone worktree"
    );

    let start = std::time::Instant::now();

    // Ensure parent and dest directories exist.
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for worktree dest: {}",
                parent.display()
            )
        })?;
    }
    std::fs::create_dir_all(&dest)
        .with_context(|| format!("failed to create dest directory: {}", dest.display()))?;

    // Run .git/ copy, modified-files scan, and working tree copy with maximum
    // parallelism. The three operations are independent:
    //   - .git/ copy:          reads source/.git/,  writes dest/.git/
    //   - modified files scan: reads source repo index + worktree (read-only)
    //   - working tree copy:   reads source/*,      writes dest/* (skips .git/)
    //
    // For PreserveWorkingTree mode, the modified-files scan is only needed for
    // the fire-and-forget index stat update, NOT for the file copy itself. So we
    // run the scan in a background thread and start the copy immediately.
    //
    // For CleanTracked/CleanAll, we need the modified files list to skip dirty
    // files during the copy, so the scan must complete before the copy begins.

    let source_git = source_root.join(".git");
    let dest_git = dest.join(".git");
    let origin_keep = copy::standalone::origin_keep_names_for_git_ref(&source_git, &git_ref);
    let origin_keep_for_copy = origin_keep.clone();

    // Spawn .git/ copy in background.
    let git_copy_handle = std::thread::Builder::new()
        .name("standalone-git-copy".to_string())
        .spawn(move || {
            let start = std::time::Instant::now();
            let stats = copy::gitdir::copy_git_dir_keeping_origin(
                &source_git,
                &dest_git,
                &origin_keep_for_copy,
            )
            .context("failed to copy .git/ directory for standalone worktree")?;
            tracing::debug!(
                elapsed = ?start.elapsed(),
                files = stats.files_copied,
                dirs = stats.dirs_created,
                skipped = stats.entries_skipped,
                ".git/ copy complete"
            );
            Ok::<_, anyhow::Error>(stats)
        })
        .context("failed to spawn .git/ copy thread")?;

    // For PreserveWorkingTree, run modified-files scan in parallel with the copy.
    // For Clean modes, run it first (we need the result to skip dirty files).
    let modified_files_for_skip;
    let modified_scan_handle;
    let mut dirty_files_report = None;

    match &working_tree {
        WorkingTreeMode::PreserveWorkingTree => {
            // Modified files are only needed for the background index stat update,
            // not the copy itself — start the scan in a background thread. On an
            // early return its handle is dropped (detached); harmless, as the
            // scan only reads the source repo and never touches `dest`.
            modified_files_for_skip = None;
            let source_root_bg = source_root.clone();
            modified_scan_handle = Some(
                std::thread::Builder::new()
                    .name("standalone-modified-scan".to_string())
                    .spawn(move || {
                        let start = std::time::Instant::now();
                        let result = git::get_modified_files(&source_root_bg)?;
                        tracing::debug!(
                            elapsed = ?start.elapsed(),
                            count = result.paths.len(),
                            "background modified files scan complete"
                        );
                        Ok::<_, anyhow::Error>(result)
                    })
                    .context("failed to spawn modified files scan thread")?,
            );
        }
        WorkingTreeMode::CleanTracked | WorkingTreeMode::CleanAll => {
            // Need modified files before copy to skip them.
            let modified_result = git::get_modified_files(&source_root)?;
            let modified_files_in_source = Arc::new(modified_result.paths);
            dirty_files_report = Some(modified_result.report);
            modified_files_for_skip = Some(Arc::clone(&modified_files_in_source));
            modified_scan_handle = None;
        }
    }

    let copy_start = std::time::Instant::now();
    let copy_config = ParallelCopyConfig {
        num_workers: effective_parallelism,
        channel_buffer,
        skip_files: modified_files_for_skip.clone(),
        respect_gitignore: true,
        skip_patterns: vec![],
    };

    let copy_result =
        copy::copy_parallel(&source_root, &dest, copy_config, cancellation_token.clone());

    // Join the background `.git/` copy thread BEFORE any teardown of `dest`, so
    // it is never still writing into `dest/.git` while we remove the directory
    // on a cancel/error path. Join unconditionally regardless of copy outcome.
    let git_stats = join_git_copy(git_copy_handle);

    let copy_result = match copy_result {
        Ok(result) => result,
        Err(e) => {
            reclaim_partial_worktree(&dest);
            return Err(e);
        }
    };
    let git_stats = match git_stats {
        Ok(stats) => stats,
        Err(e) => {
            reclaim_partial_worktree(&dest);
            return Err(e);
        }
    };

    tracing::debug!(
        elapsed = ?copy_start.elapsed(),
        files = copy_result.stats.files_copied,
        dirs = copy_result.stats.dirs_created,
        "working tree copy complete"
    );

    // Check cancellation after the heavy copy phase (threads already joined).
    if cancellation_token.is_cancelled() {
        reclaim_partial_worktree(&dest);
        anyhow::bail!("cancelled after parallel copy");
    }

    // Past this point no background thread touches `dest`; a drop guard reclaims
    // it on any finalize/checkout/commit failure.
    let guard = PartialWorktreeGuard::new(&dest);

    // Destructure before the finalize match consumes file_metadata.
    let copy_stats = copy_result.stats;
    let copied_paths = copy_result.copied_paths;
    let file_metadata = copy_result.file_metadata;

    // Phase 4: Finalize index.
    // In standalone mode, the index is already at dest/.git/index (from the .git/ CoW).
    // We just need to update stat cache entries so git status is fast.
    let finalize_start = std::time::Instant::now();
    match working_tree {
        WorkingTreeMode::PreserveWorkingTree => {
            // Join the modified-files scan (it ran in parallel with the copy).
            let modified_files_in_source = if let Some(handle) = modified_scan_handle {
                let result = handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("modified files scan thread panicked"))?
                    .context("modified files scan failed")?;
                dirty_files_report = Some(result.report);
                Arc::new(result.paths)
            } else {
                Arc::new(dashmap::DashSet::new())
            };

            // Update index stat cache so `git status` is instant.
            let clean = collect_clean_metadata(file_metadata, &modified_files_in_source);
            git::update_index_stats(&dest, &clean)?;
        }
        WorkingTreeMode::CleanTracked => {
            // `git reset --hard` rebuilds the index from HEAD with correct
            // stat caches. No need to update stats first — reset overwrites
            // the entire index unconditionally.
            drop(file_metadata);
            git::git_reset_hard_command(&dest, None)?;
        }
        WorkingTreeMode::CleanAll => {
            // Same as CleanTracked but also remove untracked files.
            drop(file_metadata);
            git::git_reset_hard_command(&dest, None)?;
            git::git_clean_fd(&dest, false)?;
        }
    }
    tracing::debug!(elapsed = ?finalize_start.elapsed(), "finalize complete");

    // Phase 5: Checkout a different ref if requested.
    if git_ref != "HEAD" {
        tracing::debug!(git_ref = %git_ref, "checking out ref");
        git::checkout_ref(&dest, &git_ref)?;
        // Dest HEAD may be detached (origin/foo). Reuse the pre-checkout keep
        // set so sanitize cannot drop the tip CoW preserved.
        copy::standalone::sanitize_standalone_git_dir_keeping(&dest.join(".git"), &origin_keep)
            .context("failed to sanitize standalone .git/ after checkout")?;
    }

    // Phase 6: Copy ignored files (optional).
    let ignored_stats = match ignored_files {
        IgnoredFilesMode::Skip | IgnoredFilesMode::CopyOnly { .. } => None,
        IgnoredFilesMode::Copy { skip_patterns } => {
            let ignored_start = std::time::Instant::now();

            let copy_config = ParallelCopyConfig {
                num_workers: effective_ignored_parallelism,
                channel_buffer,
                skip_files: Some(Arc::new(copied_paths)),
                respect_gitignore: false,
                skip_patterns,
            };

            let stats = copy::copy_parallel(&source_root, &dest, copy_config, cancellation_token)?;

            tracing::debug!(
                elapsed = ?ignored_start.elapsed(),
                files = stats.stats.files_copied,
                skipped = stats.stats.files_skipped,
                "ignored files copy complete"
            );
            Some(stats.stats)
        }
    };

    // Get the commit. If we didn't checkout a different ref, read from source
    // (avoids an extra gix::discover on the newly-created dest).
    let commit = if git_ref == "HEAD" {
        git::get_head_commit(&source_root).context("failed to get HEAD commit from source")?
    } else {
        git::get_head_commit(&dest).context("failed to get HEAD commit from dest")?
    };

    // Worktree complete — don't reclaim.
    guard.disarm();

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        files_copied = copy_stats.files_copied,
        git_dir_files = git_stats.files_copied,
        ignored_files_copied = ignored_stats.as_ref().map(|s| s.files_copied).unwrap_or(0),
        method = "standalone",
        "standalone worktree created"
    );

    Ok(CreateWorktreeResult {
        worktree_path: dest,
        commit,
        copy_stats,
        ignored_stats,
        dirty_files_report,
        resolved_strategy: crate::worktree::STRATEGY_STANDALONE,
        strategy_metadata: None,
    })
}

/// Execute worktree creation using plain `git worktree add` with checkout.
///
/// Lets git handle the entire worktree creation: creates a linked worktree,
/// checks out files, and builds the index. Uses `-c checkout.workers=N` to
/// enable parallel checkout. This is simpler than the fast-copy path and
/// avoids split-index / index-copy edge cases.
fn execute_git_checkout_worktree(plan: WorktreePlan) -> Result<CreateWorktreeResult> {
    let source = &plan.source;
    let dest = &plan.dest;
    let git_ref = &plan.git_ref;
    let workers = plan.effective_parallelism();

    let source_root = git::find_worktree_root(source)
        .with_context(|| format!("failed to find git root for {}", source.display()))?;

    tracing::info!(
        source = %source.display(),
        source_root = %source_root.display(),
        dest = %dest.display(),
        git_ref = %git_ref,
        workers,
        method = "git_checkout",
        "creating worktree via git worktree add (with checkout)"
    );

    let start = std::time::Instant::now();

    // Ensure parent directory exists.
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory for worktree dest: {}",
                parent.display()
            )
        })?;
    }

    // Run `git -c checkout.workers=N worktree add --detach <dest> <ref>`.
    // checkout.workers enables parallel checkout so git populates the
    // working tree using multiple threads.
    let output = git::checkout::git_command()
        .current_dir(&source_root)
        .arg("-c")
        .arg(format!("checkout.workers={workers}"))
        .args([
            "worktree",
            "add",
            "--detach",
            &dest.to_string_lossy(),
            git_ref,
        ])
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr);
    }

    tracing::debug!(
        elapsed = ?start.elapsed(),
        "git worktree add (with checkout) complete"
    );

    // Get the commit.
    let commit = git::get_head_commit(dest).context("failed to get HEAD commit")?;

    tracing::info!(
        elapsed = ?start.elapsed(),
        commit = %commit,
        method = "git_checkout",
        "worktree created via git checkout"
    );

    Ok(CreateWorktreeResult {
        worktree_path: dest.clone(),
        commit,
        copy_stats: CopyStats::default(),
        ignored_stats: None,
        dirty_files_report: None,
        resolved_strategy: crate::worktree::STRATEGY_GIT,
        strategy_metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_finalize_or_reclaim_snapshot_ok_skips_reclaim() {
        let reclaimed = std::cell::Cell::new(false);
        let commit =
            finalize_or_reclaim_snapshot(|| Ok("deadbeef".to_string()), || reclaimed.set(true))
                .unwrap();
        assert_eq!(commit, "deadbeef");
        assert!(
            !reclaimed.get(),
            "reclaim must not run when post ops succeed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_finalize_or_reclaim_snapshot_err_reclaims_and_propagates() {
        let reclaimed = std::cell::Cell::new(false);
        let err = finalize_or_reclaim_snapshot(
            || anyhow::bail!("post-snapshot git op failed"),
            || reclaimed.set(true),
        )
        .unwrap_err();
        assert!(err.to_string().contains("post-snapshot git op failed"));
        assert!(reclaimed.get(), "reclaim must run when post ops fail");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_overlay_upper_has_worktree_changes() {
        let tmp = TempDir::new().unwrap();
        let upper = tmp.path().join("upper");
        std::fs::create_dir_all(&upper).unwrap();

        // Empty upper => no changes => skip clean.
        assert!(!overlay_upper_has_worktree_changes(&upper));

        // Only `.git` (git metadata writes) => still no working-tree changes.
        std::fs::create_dir(upper.join(".git")).unwrap();
        std::fs::write(upper.join(".git").join("index"), b"x").unwrap();
        assert!(!overlay_upper_has_worktree_changes(&upper));

        // A copied-up/untracked top-level entry => must run clean.
        std::fs::create_dir(upper.join("crates")).unwrap();
        assert!(overlay_upper_has_worktree_changes(&upper));

        // Unreadable/missing upper => conservatively run clean.
        assert!(overlay_upper_has_worktree_changes(&tmp.path().join("nope")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_reclaim_btrfs_snapshot_removes_symlink_and_metadata() {
        use crate::btrfs;

        let tmp = TempDir::new().unwrap();
        // A fake on-disk snapshot dir (not a real subvolume — `btrfs delete` is
        // a best-effort no-op here) with sibling recovery metadata, plus the
        // symlink that exposes it at `dest`.
        let worktrees = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees).unwrap();
        let snapshot_path = worktrees.join("wt-abc");
        std::fs::create_dir(&snapshot_path).unwrap();
        let meta_path = btrfs::btrfs_meta_path(&snapshot_path).unwrap();
        std::fs::write(&meta_path, "{}").unwrap();
        let dest = tmp.path().join("dest-symlink");
        std::os::unix::fs::symlink(&snapshot_path, &dest).unwrap();

        let snap = crate::btrfs::snapshot::SnapshotResult {
            snapshot_path,
            symlink_path: Some(dest.clone()),
        };
        reclaim_btrfs_snapshot(&snap);

        // The exposing symlink and the recovery metadata are gone, so neither
        // the removal path nor the orphan scanner treats this as a live worktree.
        assert!(dest.symlink_metadata().is_err(), "symlink must be removed");
        assert!(!meta_path.exists(), "metadata must be removed");
    }

    /// Build a `WorktreePlan` whose delegate is the recording mock, for driving
    /// `try_btrfs_delegate` directly in tests.
    #[cfg(target_os = "linux")]
    fn delegate_plan(
        base: &Path,
        delegate: Arc<dyn crate::BtrfsDelegate>,
        git_ref: &str,
    ) -> WorktreePlan {
        WorktreePlan {
            source: base.join("source"),
            dest: base.join("dest"),
            git_ref: git_ref.to_string(),
            parallelism: 1,
            channel_buffer: 16,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            ignored_parallelism: 1,
            creation_mode: crate::CreationMode::Standalone,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            btrfs_delegate: Some(delegate),
            worktree_id: crate::worktree::plan::worktree_id_from_path(&base.join("dest")),
            nfs: None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_btrfs_delegate_reclaims_on_post_snapshot_failure() {
        // The motivating leak: a user-controllable non-HEAD ref whose checkout
        // fails AFTER the delegate created the snapshot must reclaim it (call
        // delete_snapshot exactly once).
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        // The delegate "exposes" a real git repo as the worktree so checkout_ref
        // runs and fails on the bogus ref (rather than failing to discover git).
        let worktree = tmp.path().join("wt");
        std::fs::create_dir(&worktree).unwrap();
        init_git_repo(&worktree);
        std::fs::write(worktree.join("f.txt"), "x").unwrap();
        git_commit_all(&worktree, "init");

        let deletes = Arc::new(AtomicUsize::new(0));
        let delegate: Arc<dyn crate::BtrfsDelegate> = Arc::new(crate::api::RecordingDelegate {
            snapshot_path: tmp.path().join("snap"),
            worktree_path: worktree.clone(),
            deletes: Arc::clone(&deletes),
        });
        let plan = delegate_plan(tmp.path(), delegate, "definitely-not-a-ref");

        let result = try_btrfs_delegate(&plan);
        assert!(
            result.is_err(),
            "bogus non-HEAD ref must fail post-snapshot ops"
        );
        assert_eq!(
            deletes.load(Ordering::Relaxed),
            1,
            "delegate snapshot must be reclaimed exactly once on failure"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn try_btrfs_delegate_no_reclaim_on_success() {
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let worktree = tmp.path().join("wt");
        std::fs::create_dir(&worktree).unwrap();
        init_git_repo(&worktree);
        std::fs::write(worktree.join("f.txt"), "x").unwrap();
        git_commit_all(&worktree, "init");

        let deletes = Arc::new(AtomicUsize::new(0));
        let delegate: Arc<dyn crate::BtrfsDelegate> = Arc::new(crate::api::RecordingDelegate {
            snapshot_path: tmp.path().join("snap"),
            worktree_path: worktree.clone(),
            deletes: Arc::clone(&deletes),
        });
        // git_ref "HEAD" (PreserveWorkingTree) → only get_head_commit runs, OK.
        let plan = delegate_plan(tmp.path(), delegate, "HEAD");

        let result = try_btrfs_delegate(&plan).unwrap();
        assert!(result.is_some(), "delegate creation should succeed");
        assert_eq!(
            deletes.load(Ordering::Relaxed),
            0,
            "no reclaim when post-snapshot ops succeed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_should_skip_overlay_polarity() {
        use crate::mount_info::MountNsStatus;
        assert!(should_skip_overlay(MountNsStatus::Private));
        assert!(!should_skip_overlay(MountNsStatus::Host));
        assert!(!should_skip_overlay(MountNsStatus::Unknown));
    }

    /// Create a fake .git directory with lock files and transient state,
    /// simulating what a btrfs snapshot would capture.
    fn setup_poisoned_git_dir(git_dir: &Path) {
        std::fs::create_dir_all(git_dir.join("objects/pack")).unwrap();
        std::fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git_dir.join("config"), "[core]\nbare = false\n").unwrap();
        std::fs::write(git_dir.join("index"), "fake index data").unwrap();
        std::fs::write(git_dir.join("refs/heads/main"), "abc123\n").unwrap();

        // Poison: lock files
        std::fs::write(git_dir.join("index.lock"), "").unwrap();
        std::fs::write(git_dir.join("config.lock"), "").unwrap();
        std::fs::write(git_dir.join("refs/heads/main.lock"), "").unwrap();

        // Poison: transient state
        std::fs::write(git_dir.join("MERGE_HEAD"), "abc123").unwrap();
        std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), "def456").unwrap();
        std::fs::write(git_dir.join("ORIG_HEAD"), "aaa111").unwrap();
        std::fs::write(git_dir.join("FETCH_HEAD"), "bbb222").unwrap();
        std::fs::write(git_dir.join("REBASE_HEAD"), "ccc333").unwrap();

        // Poison: in-progress operation directories
        std::fs::create_dir_all(git_dir.join("rebase-merge")).unwrap();
        std::fs::write(git_dir.join("rebase-merge/head-name"), "main").unwrap();
        std::fs::create_dir_all(git_dir.join("sequencer")).unwrap();
        std::fs::write(git_dir.join("sequencer/todo"), "pick abc123").unwrap();
        std::fs::create_dir_all(git_dir.join("rebase-apply")).unwrap();
        std::fs::write(git_dir.join("rebase-apply/applying"), "1").unwrap();
    }

    #[test]
    fn test_cleanup_removes_lock_files() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("worktree");
        let git_dir = wt.join(".git");
        std::fs::create_dir_all(&wt).unwrap();
        setup_poisoned_git_dir(&git_dir);

        let removed = cleanup_snapshot_git_state(&wt);
        assert!(removed > 0, "should have removed something");

        // Lock files should be gone
        assert!(!git_dir.join("index.lock").exists());
        assert!(!git_dir.join("config.lock").exists());
        assert!(!git_dir.join("refs/heads/main.lock").exists());

        // Essential files should remain
        assert!(git_dir.join("HEAD").exists());
        assert!(git_dir.join("config").exists());
        assert!(git_dir.join("index").exists());
        assert!(git_dir.join("refs/heads/main").exists());
        assert!(git_dir.join("objects/pack").exists());
    }

    #[test]
    fn test_cleanup_removes_transient_state() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("worktree");
        let git_dir = wt.join(".git");
        std::fs::create_dir_all(&wt).unwrap();
        setup_poisoned_git_dir(&git_dir);

        cleanup_snapshot_git_state(&wt);

        // Transient state files should be gone
        assert!(!git_dir.join("MERGE_HEAD").exists());
        assert!(!git_dir.join("CHERRY_PICK_HEAD").exists());
        assert!(!git_dir.join("ORIG_HEAD").exists());
        assert!(!git_dir.join("FETCH_HEAD").exists());
        assert!(!git_dir.join("REBASE_HEAD").exists());

        // In-progress operation directories should be gone
        assert!(!git_dir.join("rebase-merge").exists());
        assert!(!git_dir.join("sequencer").exists());
        assert!(!git_dir.join("rebase-apply").exists());
    }

    #[test]
    fn test_cleanup_noop_for_linked_worktree() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        // .git is a file (linked worktree), not a directory
        std::fs::write(wt.join(".git"), "gitdir: /some/path/.git/worktrees/wt").unwrap();

        let removed = cleanup_snapshot_git_state(&wt);
        assert_eq!(removed, 0, "should not clean linked worktrees");
    }

    #[test]
    fn test_cleanup_noop_for_clean_git_dir() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("worktree");
        let git_dir = wt.join(".git");
        std::fs::create_dir_all(git_dir.join("objects")).unwrap();
        std::fs::create_dir_all(git_dir.join("refs")).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git_dir.join("index"), "clean index").unwrap();

        let removed = cleanup_snapshot_git_state(&wt);
        assert_eq!(removed, 0, "clean git dir should need no cleanup");

        // Everything still intact
        assert!(git_dir.join("HEAD").exists());
        assert!(git_dir.join("index").exists());
    }

    #[test]
    fn test_cleanup_skips_objects_directory() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join("worktree");
        let git_dir = wt.join(".git");
        std::fs::create_dir_all(git_dir.join("objects/pack")).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        // Put a .lock file inside objects/ (shouldn't happen, but verify we don't walk it)
        std::fs::write(git_dir.join("objects/pack/fake.lock"), "").unwrap();

        // Also add a real lock file to verify cleanup works
        std::fs::write(git_dir.join("index.lock"), "").unwrap();

        let removed = cleanup_snapshot_git_state(&wt);
        assert_eq!(
            removed, 1,
            "should only remove index.lock, not objects/pack/fake.lock"
        );
        assert!(!git_dir.join("index.lock").exists());
        assert!(
            git_dir.join("objects/pack/fake.lock").exists(),
            "should NOT walk into objects/"
        );
    }

    #[test]
    fn test_marker_written_for_standalone_git_dir() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        echo_agent_test_utils::git::init_git_repo(&source);

        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(dest.join(".git")).unwrap();

        record_main_repo_marker(&source, &dest);

        let marker = dest.join(".git/echo-agent-worktree-source");
        let recorded = std::fs::read_to_string(&marker).expect("marker should be written");
        assert!(
            Path::new(recorded.trim()).join(".git").is_dir(),
            "recorded path should point at the source repo, got {recorded:?}"
        );
    }

    #[test]
    fn test_marker_skipped_for_linked_worktree() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join(".git"), "gitdir: /main/.git/worktrees/wt").unwrap();

        record_main_repo_marker(&source, &dest);

        assert!(!dest.join(".git/echo-agent-worktree-source").exists());
    }

    #[test]
    fn test_marker_not_overwritten_when_present() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        echo_agent_test_utils::git::init_git_repo(&source);

        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(dest.join(".git")).unwrap();
        let marker = dest.join(".git/echo-agent-worktree-source");
        std::fs::write(&marker, "/the/ultimate/main/repo").unwrap();

        record_main_repo_marker(&source, &dest);

        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "/the/ultimate/main/repo",
            "existing marker must not be overwritten"
        );
    }
}
