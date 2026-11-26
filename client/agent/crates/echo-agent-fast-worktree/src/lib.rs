#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! High-performance git worktree creation using CoW cloning.
//!
//! This crate provides fast worktree creation by:
//! 1. Using `git worktree add --no-checkout` (instant metadata creation)
//! 2. Parallel CoW file cloning with hash-based sharding
//! 3. Optional dirty file replication and ignored file copying
//! 4. BTRFS snapshot support on Linux for O(1) cloning
//! 5. Worktree sync API for pre-created worktree pools
//! 6. SQLite metadata tracking (behind `metadata` feature)
mod api;
#[cfg(feature = "metadata")]
mod auto_gc;
#[cfg(target_os = "linux")]
pub mod btrfs;
mod copy;
#[cfg(feature = "metadata")]
pub mod db;
#[cfg(feature = "metadata")]
pub mod discovery;
mod git;
mod metrics;
#[cfg(target_os = "linux")]
pub(crate) mod mount_info;
#[cfg(unix)]
mod nfs;
#[cfg(not(unix))]
#[path = "nfs_stub.rs"]
mod nfs;
#[cfg(target_os = "linux")]
mod overlay;
pub mod sync;
#[cfg(test)]
mod test_support;
pub(crate) mod time;
#[cfg(target_os = "linux")]
pub(crate) mod util;
mod worktree;
#[cfg(target_os = "linux")]
pub use api::cleanup_orphaned_btrfs_snapshots;
#[cfg(target_os = "linux")]
pub use api::cleanup_orphaned_overlay_snapshots;
#[cfg(feature = "metadata")]
pub use api::gc::{GcOptions, GcReport, KeptWorktree, gc_worktrees, gc_worktrees_with_delegate};
pub use api::{
    BtrfsDelegate, BtrfsMode, CleanupReport, CopyReport, CreationMode, DelegateSnapshotResult,
    DirtyFilesReport, ENOSPC_OS_MESSAGE, IgnoredFilesMode, OUT_OF_DISK_CONTEXT, RemoveReport,
    WorkingTreeMode, WorktreeBuilder, WorktreeReport, cleanup_worktrees_in,
    cleanup_worktrees_in_with_delegate, remove_worktree, remove_worktree_with_delegate,
};
#[cfg(feature = "metadata")]
pub use auto_gc::{
    AutoGcOutcome, AutoGcReport, ENV_AUTO_GC, ENV_AUTO_GC_DRY_RUN, ENV_AUTO_GC_MAX_AGE,
    ENV_AUTO_GC_REBUILD, ResolvedWorktreeAutoGc, WorktreeAutoGcLayer, clear_auto_gc_env_for_test,
    maybe_auto_gc, resolve_worktree_auto_gc_from_layers, run_auto_gc_pass,
};
#[cfg(feature = "metadata")]
pub use db::{
    DbStats, ListFilter, META_KEY_LABEL, RegistryOpen, SqliteFailureKind, WorktreeDb, WorktreeKind,
    WorktreeRecord, WorktreeStatus, classify_sqlite_error, now_epoch_secs, resolve_echo_agent_home,
};
#[cfg(feature = "metadata")]
pub use discovery::{
    RebuildReport, WORKTREE_DEPTH, WORKTREE_POOL_DIR, WORKTREES_DIR, discover_worktrees,
    managed_worktree_roots, path_under_managed_worktree_roots, path_under_worktree_roots,
    rebuild_worktree_db, rebuild_worktree_db_with_grove_data,
};
pub use git::checkout::{
    rehydrate_worktree_from_ref, snapshot_worktree_to_ref, transfer_snapshot_to_repo,
};
pub use git::{
    KeepReason, Reclaim, reclaimable_after_snapshot, remove_stale_worktree_registration,
    remove_stale_worktree_registrations_under,
};
pub use metrics::{
    grove_wt_create_count, grove_wt_create_last_duration_ns, record_grove_wt_create,
};
pub use nfs::create_latency_stamp;
pub use nfs::{
    CleanArtifactsReply, DetachReply, NfsAdopted, NfsCreateDecision, NfsStatusView,
    NfsWorktreeClient, NfsWorktreeOpts, SalvageReply, dest_is_mountpoint, dest_is_nfs_mount,
};
pub fn local_salvage(
    _dest: &std::path::Path,
    _out: &std::path::Path,
) -> anyhow::Result<SalvageReply> {
    anyhow::bail!("not available in this build")
}
pub fn local_clean_artifacts(_dest: &std::path::Path) -> anyhow::Result<CleanArtifactsReply> {
    anyhow::bail!("not available in this build")
}
pub use sync::{SourceDirtyState, SyncReport, WorktreeSync, collect_source_dirty_state};
#[cfg(target_os = "linux")]
pub use worktree::execute::cleanup_snapshot_git_state;
pub use worktree::{STRATEGY_GROVE_FUSE, STRATEGY_GROVE_NFS, STRATEGY_NFS, is_grove_strategy};
/// Count the number of tracked files in a git repository's index.
///
/// Reads the index header via `gix`, which contains the entry count — this
/// is an O(1) read (no directory walk). Useful for deciding whether a repo
/// is large enough to benefit from worktree pooling.
pub fn count_tracked_files(repo_path: &std::path::Path) -> anyhow::Result<usize> {
    let repo = gix::discover(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to discover git repo: {e}"))?;
    let index = repo
        .index_or_load_from_head()
        .map_err(|e| anyhow::anyhow!("failed to load git index: {e}"))?;
    Ok(index.entries().len())
}
