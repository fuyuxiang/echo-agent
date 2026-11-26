//! Public API for fast worktree creation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Serializes tests that chdir or assert process-CWD scan results (process-global cwd).
/// Gated on `metadata` because every caller lives under that feature's test modules
/// (`gc` / `auto_gc`); without the feature these would be dead under `-D warnings`.
#[cfg(all(test, feature = "metadata"))]
pub(crate) static CWD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(test, feature = "metadata"))]
pub(crate) fn cwd_test_guard() -> std::sync::MutexGuard<'static, ()> {
    CWD_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Restores process cwd on drop (pair with [`cwd_test_guard`]).
#[cfg(all(test, feature = "metadata"))]
pub(crate) struct CwdGuard(pub PathBuf);

#[cfg(all(test, feature = "metadata"))]
impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::copy::CopyStats;
pub use crate::copy::DirtyFilesReport;
use crate::copy::ParallelCopyConfig;
pub use crate::nfs::NfsWorktreeOpts;

/// Result from a delegated btrfs snapshot creation.
#[derive(Debug, Clone)]
pub struct DelegateSnapshotResult {
    pub snapshot_path: PathBuf,
    /// Path where the worktree is accessible (bind-mounted from `snapshot_path`).
    pub worktree_path: PathBuf,
    /// Whether a bind mount was created from `snapshot_path` to `worktree_path`.
    pub bind_mounted: bool,
}

/// Delegate privileged btrfs operations to an external helper.
///
/// When the caller runs inside a sandbox without `CAP_SYS_ADMIN`, it cannot
/// execute `btrfs subvolume snapshot/delete` directly. This trait lets it
/// delegate those operations to a privileged process (e.g. over IPC).
///
/// Implementations must be `Send + Sync` (shared across threads).
pub trait BtrfsDelegate: Send + Sync {
    /// Create a btrfs snapshot of `source` accessible at `dest`.
    ///
    /// The implementation is expected to:
    /// 1. Detect whether `source` is a btrfs subvolume
    /// 2. Create a snapshot (inside the btrfs filesystem)
    /// 3. Bind mount `dest` from snapshot if source is bind-mounted
    /// 4. Clean up stale git state (lock files, worktree registrations)
    fn create_snapshot(&self, source: &Path, dest: &Path) -> Result<DelegateSnapshotResult>;

    /// Delete a btrfs snapshot worktree.
    ///
    /// If `worktree_path` is a bind mount, the implementation should unmount it,
    /// delete the btrfs snapshot, and clean up the mount point.
    fn delete_snapshot(&self, worktree_path: &Path) -> Result<RemoveReport>;

    /// Mount an overlayfs at `target` in the *caller's* mount namespace.
    ///
    /// A FUSE+overlay worktree needs a new overlay mount, which a rootless
    /// caller can't do (no `CAP_SYS_ADMIN`); the privileged delegate mounts it
    /// inside the caller's namespace (an overlay mount can't be exposed via a
    /// namespace-crossing symlink the way a btrfs snapshot can). Default impl
    /// errors so btrfs-only delegates still compile.
    fn mount_overlay(&self, lower: &Path, upper: &Path, work: &Path, target: &Path) -> Result<()> {
        let _ = (lower, upper, work, target);
        anyhow::bail!("overlay mount delegation not supported by this delegate")
    }

    /// Unmount an overlay worktree previously mounted via [`Self::mount_overlay`]
    /// (in the caller's mount namespace).
    fn unmount_overlay(&self, target: &Path) -> Result<()> {
        let _ = target;
        anyhow::bail!("overlay unmount delegation not supported by this delegate")
    }
}

/// How to treat the source working tree when creating the destination worktree.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WorkingTreeMode {
    /// Replicate the working tree exactly as-is (including local modifications and untracked files).
    #[default]
    PreserveWorkingTree,
    /// Produce a clean checked-out working tree for tracked files.
    ///
    /// Local modifications and untracked files from the source are not copied.
    CleanTracked,
    /// Produce a clean worktree and also remove any untracked files (equivalent to
    /// `git reset --hard` + `git clean -fd`).
    ///
    /// Note: ignored files are not removed by default `git clean`.
    CleanAll,
}

/// Whether (and how) to copy `.gitignore`'d files after the worktree is ready.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum IgnoredFilesMode {
    /// Do not copy ignored files.
    #[default]
    Skip,
    /// Copy ignored files, optionally skipping additional patterns.
    Copy { skip_patterns: Vec<String> },
    /// Copy ONLY ignored files (no worktree creation), optionally skipping additional patterns.
    /// This is for standalone use via `copy_ignored_only()`.
    CopyOnly { skip_patterns: Vec<String> },
}

/// How to handle BTRFS snapshot optimization on Linux.
///
/// On Linux systems where the source repo is on a BTRFS subvolume,
/// we can use BTRFS snapshots for O(1) worktree creation instead of
/// file-by-file CoW cloning.
///
/// The snapshot creates a complete standalone git repository (not a
/// linked git worktree), which is immediately usable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum BtrfsMode {
    /// Auto-detect: use BTRFS snapshot if source is on a BTRFS subvolume.
    /// Falls back to file-by-file copy if not on BTRFS or not a subvolume.
    #[default]
    Auto,
    /// Force use of BTRFS snapshot. Returns an error if the source is not
    /// on a BTRFS subvolume.
    Force,
    /// Disable BTRFS snapshot optimization. Always use file-by-file copy.
    Disabled,
}

/// Strategy for creating the worktree.
///
/// Consolidates the choice of linked vs standalone, BTRFS snapshots,
/// and git-native checkout into a single enum.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CreationMode {
    /// Linked worktree via `git worktree add --no-checkout` followed by
    /// parallel CoW file copy and index finalization. On Linux with BTRFS,
    /// auto-detects and uses instant snapshots when possible.
    ///
    /// This is the fastest mode for large repos on APFS/Btrfs.
    #[default]
    Linked,

    /// Standalone repository copy with its own independent `.git/`
    /// directory (CoW'd from the source). Can be promoted to replace the
    /// source via a simple `rename()`, with no worktree cleanup needed.
    ///
    /// On Linux with BTRFS, auto-detects and uses instant snapshots.
    Standalone,

    /// Plain `git worktree add` with full checkout. Lets git handle the
    /// entire worktree creation including index and working tree
    /// population. Simpler and avoids split-index / index-copy edge
    /// cases, but git does the checkout single-threaded.
    GitCheckout,
}

impl CreationMode {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Standalone => "standalone",
            Self::GitCheckout => "git",
        }
    }
}

/// A structured report for a copy phase.
#[derive(Clone, Debug, Default)]
pub struct CopyReport {
    pub files_copied: u64,
    pub dirs_created: u64,
    pub symlinks_copied: u64,
    pub files_skipped: u64,
    /// Non-fatal issues encountered during copying.
    pub issues: Vec<String>,
    pub dirty_files: Option<DirtyFilesReport>,
}

impl From<CopyStats> for CopyReport {
    fn from(stats: CopyStats) -> Self {
        Self {
            files_copied: stats.files_copied,
            dirs_created: stats.dirs_created,
            symlinks_copied: stats.symlinks_copied,
            files_skipped: stats.files_skipped,
            issues: stats.issues,
            dirty_files: None,
        }
    }
}

/// Result of creating a worktree via the new API.
#[derive(Debug)]
pub struct WorktreeReport {
    pub worktree_path: PathBuf,
    pub commit: String,
    pub unignored_copy: CopyReport,
    pub ignored_copy: Option<CopyReport>,
    /// Dispatch arm that actually ran (`nfs` / `overlay` / `btrfs` / `copy` / `git` / `standalone`).
    pub resolved_strategy: &'static str,
    /// Arm-specific metadata persisted into worktrees.db.
    pub strategy_metadata: Option<serde_json::Value>,
}

/// High-level builder API for creating fast git worktrees.
///
/// All operations are **synchronous/blocking**. Callers should use `spawn_blocking`
/// when calling from async contexts.
#[derive(Clone)]
pub struct WorktreeBuilder {
    source: PathBuf,
    dest: PathBuf,
    git_ref: String,
    parallelism: usize,
    channel_buffer: usize,
    ignored_parallelism: usize,
    working_tree: WorkingTreeMode,
    ignored_files: IgnoredFilesMode,
    creation_mode: CreationMode,
    cancellation_token: CancellationToken,
    btrfs_delegate: Option<Arc<dyn BtrfsDelegate>>,
    #[cfg(feature = "metadata")]
    worktree_kind: Option<crate::db::WorktreeKind>,
    #[cfg(feature = "metadata")]
    session_id: Option<String>,
    #[cfg(feature = "metadata")]
    worktree_id: Option<String>,
    #[cfg(feature = "metadata")]
    metadata: Option<serde_json::Value>,
    nfs: Option<NfsWorktreeOpts>,
}

impl std::fmt::Debug for WorktreeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorktreeBuilder")
            .field("source", &self.source)
            .field("dest", &self.dest)
            .field("git_ref", &self.git_ref)
            .field("parallelism", &self.parallelism)
            .field("creation_mode", &self.creation_mode)
            .field("btrfs_delegate", &self.btrfs_delegate.is_some())
            .finish_non_exhaustive()
    }
}

impl WorktreeBuilder {
    pub fn new(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            dest: dest.into(),
            git_ref: "HEAD".to_string(),
            parallelism: 0,
            channel_buffer: 256,
            ignored_parallelism: 0,
            working_tree: WorkingTreeMode::PreserveWorkingTree,
            ignored_files: IgnoredFilesMode::Skip,
            creation_mode: CreationMode::default(),
            cancellation_token: CancellationToken::new(),
            btrfs_delegate: None,
            #[cfg(feature = "metadata")]
            worktree_kind: None,
            #[cfg(feature = "metadata")]
            session_id: None,
            #[cfg(feature = "metadata")]
            worktree_id: None,
            #[cfg(feature = "metadata")]
            metadata: None,
            nfs: None,
        }
    }

    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    pub fn git_ref(mut self, git_ref: impl Into<String>) -> Self {
        self.git_ref = git_ref.into();
        self
    }

    pub fn parallelism(mut self, parallelism: usize) -> Self {
        self.parallelism = parallelism;
        self
    }

    pub fn ignored_parallelism(mut self, parallelism: usize) -> Self {
        self.ignored_parallelism = parallelism;
        self
    }

    pub fn channel_buffer(mut self, channel_buffer: usize) -> Self {
        self.channel_buffer = channel_buffer;
        self
    }

    pub fn working_tree_mode(mut self, mode: WorkingTreeMode) -> Self {
        self.working_tree = mode;
        self
    }

    pub fn ignored_files_mode(mut self, mode: IgnoredFilesMode) -> Self {
        self.ignored_files = mode;
        self
    }

    /// Set the worktree creation strategy.
    ///
    /// - `Linked` (default): `git worktree add --no-checkout` + parallel
    ///   CoW file copy + index finalization. Fastest on large repos.
    /// - `Standalone`: Independent `.git/` copy (CoW'd). Can be promoted
    ///   to replace the source via `rename()`.
    /// - `GitCheckout`: Plain `git worktree add` with full checkout. Simpler,
    ///   avoids split-index issues, but single-threaded checkout.
    pub fn creation_mode(mut self, mode: CreationMode) -> Self {
        self.creation_mode = mode;
        self
    }

    /// Set the worktree kind for metadata tracking.
    /// When set, `create()` auto-registers the worktree in the metadata DB.
    #[cfg(feature = "metadata")]
    pub fn worktree_kind(mut self, kind: crate::db::WorktreeKind) -> Self {
        self.worktree_kind = Some(kind);
        self
    }

    #[cfg(feature = "metadata")]
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Override the worktree ID (default: derived from dest path).
    #[cfg(feature = "metadata")]
    pub fn worktree_id(mut self, id: impl Into<String>) -> Self {
        self.worktree_id = Some(id.into());
        self
    }

    #[cfg(feature = "metadata")]
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Shorthand for `.creation_mode(CreationMode::Standalone)`.
    pub fn standalone(mut self, standalone: bool) -> Self {
        if standalone {
            self.creation_mode = CreationMode::Standalone;
        }
        self
    }

    /// Shorthand for setting the BTRFS snapshot mode (Linux only).
    ///
    /// BTRFS snapshots are automatically used by `Linked` and `Standalone`
    /// modes when the source is on a BTRFS subvolume. This method is only
    /// needed to *force* or *disable* that auto-detection.
    pub fn btrfs_mode(self, mode: BtrfsMode) -> Self {
        // This method is kept for backward compatibility with the CLI.
        tracing::warn!(
            ?mode,
            "WorktreeBuilder::btrfs_mode() is deprecated and has no effect. \
             BtrfsMode is now handled automatically based on CreationMode."
        );
        self
    }

    /// Set a delegate for privileged btrfs operations.
    ///
    /// When the caller lacks `CAP_SYS_ADMIN` (e.g., inside a bwrap sandbox),
    /// btrfs snapshot creation/deletion can be delegated to a privileged
    /// process via this trait. The delegate is tried as a fallback when
    /// direct btrfs operations fail or are unavailable.
    pub fn btrfs_delegate(mut self, delegate: Arc<dyn BtrfsDelegate>) -> Self {
        self.btrfs_delegate = Some(delegate);
        self
    }

    /// Explicit grove worktree enablement (macOS NFS / Linux FUSE).
    /// The library never reads pager config.
    pub fn grove_worktree(mut self, opts: NfsWorktreeOpts) -> Self {
        self.nfs = Some(opts);
        self
    }

    /// Deprecated alias for [`Self::grove_worktree`].
    pub fn nfs_worktree(self, opts: NfsWorktreeOpts) -> Self {
        self.grove_worktree(opts)
    }

    /// Create the worktree using the configured options.
    ///
    /// This is a **blocking** operation. Callers should use `spawn_blocking`
    /// when calling from async contexts.
    pub fn create(self) -> Result<WorktreeReport> {
        // One canonical dest for the plan id, IPC idempotency key, and DB id.
        let dest = crate::worktree::plan::canonicalize_for_id(&self.dest);
        let worktree_id = {
            #[cfg(feature = "metadata")]
            {
                self.worktree_id
                    .unwrap_or_else(|| crate::worktree::plan::worktree_id_from_path(&dest))
            }
            #[cfg(not(feature = "metadata"))]
            {
                crate::worktree::plan::worktree_id_from_path(&dest)
            }
        };
        if !crate::nfs::is_safe_worktree_id(&worktree_id) {
            anyhow::bail!("invalid worktree id from dest: {worktree_id}");
        }

        #[cfg(feature = "metadata")]
        let meta_fields = (
            self.worktree_kind,
            self.session_id,
            worktree_id.clone(),
            self.source.clone(),
            self.git_ref.clone(),
            self.metadata,
        );

        let plan = crate::worktree::WorktreePlan {
            source: self.source,
            dest,
            git_ref: self.git_ref,
            parallelism: self.parallelism,
            channel_buffer: self.channel_buffer,
            working_tree: self.working_tree,
            ignored_files: self.ignored_files,
            ignored_parallelism: self.ignored_parallelism,
            creation_mode: self.creation_mode,
            cancellation_token: self.cancellation_token,
            btrfs_delegate: self.btrfs_delegate,
            worktree_id,
            nfs: self.nfs,
        };

        let result = crate::worktree::execute_plan(plan).map_err(annotate_disk_full)?;

        #[cfg(feature = "metadata")]
        {
            let (kind, session_id, wt_id, source, git_ref, mut metadata) = meta_fields;
            if let Some(kind) = kind {
                if let Some(sm) = result.strategy_metadata.clone() {
                    metadata = Some(merge_strategy_metadata(metadata, sm));
                }
                register_worktree(
                    &result.worktree_path,
                    &source,
                    kind,
                    result.resolved_strategy,
                    &git_ref,
                    &result.commit,
                    session_id,
                    Some(wt_id),
                    metadata,
                );
            }
        }

        let mut unignored_copy: CopyReport = result.copy_stats.into();
        unignored_copy.dirty_files = result.dirty_files_report;

        Ok(WorktreeReport {
            worktree_path: result.worktree_path,
            commit: result.commit,
            unignored_copy,
            ignored_copy: result.ignored_stats.map(Into::into),
            resolved_strategy: result.resolved_strategy,
            strategy_metadata: result.strategy_metadata,
        })
    }

    /// Copy ONLY `.gitignore`'d (ignored) files from `source` to `dest`.
    ///
    /// This does **not** create or finalize a worktree. It's intended to be run after a
    /// worktree already exists at `dest`, to populate ignored artifacts (node_modules, target, etc.).
    ///
    /// This is a **blocking** operation. Callers should use `spawn_blocking`
    /// when calling from async contexts.
    pub fn copy_ignored_only(self) -> Result<CopyReport> {
        let source = &self.source;
        let dest = &self.dest;

        let num_workers = if self.ignored_parallelism != 0 {
            self.ignored_parallelism
        } else if self.parallelism != 0 {
            self.parallelism
        } else {
            num_cpus::get()
        };

        let skip_patterns = match self.ignored_files {
            IgnoredFilesMode::Skip => vec![],
            IgnoredFilesMode::Copy { skip_patterns } => skip_patterns,
            IgnoredFilesMode::CopyOnly { skip_patterns } => skip_patterns,
        };

        tracing::info!(
            source = %source.display(),
            dest = %dest.display(),
            parallelism = num_workers,
            channel_buffer = self.channel_buffer,
            "copying ignored files (ignored-only)"
        );

        let start = std::time::Instant::now();
        let unignored_paths = crate::copy::collect_unignored_paths(source, num_workers)?;

        let copy_config = ParallelCopyConfig {
            num_workers,
            channel_buffer: self.channel_buffer,
            skip_files: Some(Arc::new(unignored_paths)),
            respect_gitignore: false,
            skip_patterns,
        };

        let copy_result =
            crate::copy::copy_parallel(source, dest, copy_config, self.cancellation_token.clone())?;

        // `copy_parallel` returns Ok with partial stats on cancellation; surface
        // it so an interrupted copy isn't treated as success.
        if self.cancellation_token.is_cancelled() {
            anyhow::bail!("cancelled during ignored-only copy");
        }

        tracing::debug!(
            elapsed = ?start.elapsed(),
            files = copy_result.stats.files_copied,
            dirs = copy_result.stats.dirs_created,
            symlinks = copy_result.stats.symlinks_copied,
            skipped = copy_result.stats.files_skipped,
            "copying ignored files (ignored-only) complete"
        );

        Ok(copy_result.stats.into())
    }
}

/// Error context attached when worktree creation fails on a full disk. The
/// pager matches on it, so this constant is the cross-crate contract.
pub const OUT_OF_DISK_CONTEXT: &str = "not enough free disk space";

/// POSIX disk-full text `git` prints to stderr; the text fallback for the
/// typed `ErrorKind::StorageFull` check.
pub const ENOSPC_OS_MESSAGE: &str = "No space left on device";

/// Detect a disk-full failure anywhere in an error chain.
///
/// Worktree creation touches the disk in many places (reflink/copy of files
/// and the git index, directory creation, `git worktree add`). When the volume
/// fills up the underlying `std::io::Error` reports `ErrorKind::StorageFull`:
/// std maps `ENOSPC` (Linux/macOS) and `ERROR_DISK_FULL` /
/// `ERROR_HANDLE_DISK_FULL` (Windows) onto it, so this is correct on every
/// platform. `git` subcommands instead surface the failure only as stderr text.
fn is_out_of_disk(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::StorageFull
        {
            return true;
        }
        // Fallback for `git` subcommands, which report this only as stderr text.
        cause.to_string().contains(ENOSPC_OS_MESSAGE)
    })
}

/// Promote a disk-full reason to the top of the error chain.
///
/// Downstream layers (the workspace hub, ACP) flatten the `anyhow` chain to its
/// top-level message via `Display`, discarding the root `io::Error`. Without
/// this, a full disk surfaces to the user as an opaque
/// `"failed to copy index from … to …"`. Promoting the reason to the outermost
/// context ensures it survives that flattening; the original chain is preserved
/// underneath for logs (`{:#}` / `{:?}`).
fn annotate_disk_full(err: anyhow::Error) -> anyhow::Error {
    if is_out_of_disk(&err) {
        err.context(OUT_OF_DISK_CONTEXT)
    } else {
        err
    }
}

#[cfg(feature = "metadata")]
fn merge_strategy_metadata(
    caller: Option<serde_json::Value>,
    strategy: serde_json::Value,
) -> serde_json::Value {
    match (caller, strategy) {
        (Some(serde_json::Value::Object(mut a)), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            serde_json::Value::Object(a)
        }
        (Some(c), _) if c.is_object() => c,
        (_, s) => s,
    }
}

/// Result of removing a worktree.
#[derive(Clone, Debug)]
pub struct RemoveReport {
    /// Whether a btrfs subvolume delete was used (O(1)) vs git worktree remove (O(n)).
    pub used_btrfs_delete: bool,
    pub unmounted_bind: bool,
    pub unmounted_overlay: bool,
}

/// Remove a worktree, using the fastest available method.
///
/// Detection order:
/// 1. If the worktree is a symlink/bind-mount to a btrfs snapshot, or a direct btrfs subvolume → unmount if needed + `btrfs subvolume delete` (O(1))
/// 2. Otherwise → `rm -rf` + deregister from `.git/worktrees/`
///
/// **Why not `git worktree remove --force`?** On large repos (100K+ files),
/// `git worktree remove` walks all files to delete them (often tens of seconds).
/// Using `rm -rf` + deregistration is ~10x faster because the kernel handles
/// bulk deletion more efficiently, and we avoid git's per-file validation.
///
/// This is a **blocking** operation. Callers should use `spawn_blocking`
/// when calling from async contexts.
pub fn remove_worktree(worktree_path: &std::path::Path) -> Result<RemoveReport> {
    remove_worktree_inner(worktree_path, None)
}

/// Remove a worktree with an optional delegate for privileged btrfs operations.
///
/// When the caller has a `BtrfsDelegate` (e.g., from a sandbox with IPC to a
/// privileged helper), this function uses it as a fallback when direct btrfs
/// operations fail (e.g., due to missing `CAP_SYS_ADMIN`).
pub fn remove_worktree_with_delegate(
    worktree_path: &std::path::Path,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
) -> Result<RemoveReport> {
    remove_worktree_inner(worktree_path, delegate.as_ref())
}

fn remove_worktree_inner(
    worktree_path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<RemoveReport> {
    let report = remove_worktree_from_disk(worktree_path, delegate)?;

    // Unregister only AFTER a successful on-disk removal: a failed removal (e.g.
    // EPERM on btrfs delete) must keep the record so the worktree stays tracked
    // by list/gc instead of leaking untracked on disk.
    #[cfg(feature = "metadata")]
    unregister_worktree(worktree_path);

    Ok(report)
}

/// Remove the worktree from disk (overlay/btrfs/metadata fast paths or `rm -rf`
/// + deregister), without touching the metadata DB. Returns `Err` if the on-disk
/// removal fails, so the caller can keep the DB record.
fn remove_worktree_from_disk(
    worktree_path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<RemoveReport> {
    use anyhow::Context;

    #[cfg(not(target_os = "linux"))]
    let _ = delegate;

    // NFS: daemon-first verified unmount. Never `umount -f`, never rm -rf a live mount.
    {
        match crate::nfs::try_nfs_remove(worktree_path) {
            Ok(Some(report)) => return Ok(report),
            Ok(None) => {}
            Err(e) => {
                // Fail closed for any NFS arm Err (inconclusive mount table, live non-grove NFS,
                // or post-marker teardown). Swallowing would let the caller rm -rf a dest that
                // may still be mounted or only partially cleaned.
                return Err(e);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(report) = try_overlay_remove(worktree_path, delegate)? {
            return Ok(report);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(report) = try_btrfs_remove_from_metadata(worktree_path, delegate)? {
            return Ok(report);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(report) = try_btrfs_remove(worktree_path, delegate)? {
            return Ok(report);
        }
    }

    tracing::debug!(
        path = %worktree_path.display(),
        "removing worktree via rm -rf + deregister"
    );

    // Read the registration dir from the worktree's `.git` BEFORE deleting it.
    let registration_dir = read_worktree_gitdir(worktree_path);

    // symlink_metadata, not `exists()` (which follows the link): a worktree
    // exposed as a symlink, including a now-dangling one, must be unlinked, not
    // skipped. (On Linux, symlinks are normally handled earlier in try_btrfs_remove.)
    match std::fs::symlink_metadata(worktree_path) {
        Ok(md) if md.file_type().is_symlink() => {
            std::fs::remove_file(worktree_path).context(format!(
                "failed to remove worktree symlink: {}",
                worktree_path.display()
            ))?;
        }
        Ok(_) => {
            std::fs::remove_dir_all(worktree_path).context(format!(
                "failed to remove worktree directory: {}",
                worktree_path.display()
            ))?;
        }
        Err(_) => {} // nothing at the path
    }

    if let Some(reg_dir) = registration_dir
        && reg_dir.exists()
    {
        // The `.git` pointer is untrusted, so deregister only a `.git/worktrees/<name>`
        // entry whose own `gitdir` backlink resolves back to this worktree. Neither
        // condition alone is enough: shape rejects arbitrary dirs, backlink rejects siblings.
        let is_registration_dir =
            reg_dir.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new("worktrees"));
        let backlinks_here = crate::git::registration_worktree_path(&reg_dir)
            == Some(crate::git::normalized_for_match(worktree_path));
        if is_registration_dir && backlinks_here {
            tracing::debug!(
                registration_dir = %reg_dir.display(),
                "removing worktree registration from .git/worktrees/"
            );
            let _ = std::fs::remove_dir_all(&reg_dir);
        } else {
            tracing::warn!(
                registration_dir = %reg_dir.display(),
                "skipping registration cleanup: not a worktrees entry backlinking to this worktree"
            );
        }
    }

    Ok(RemoveReport {
        used_btrfs_delete: false,
        unmounted_bind: false,
        unmounted_overlay: false,
    })
}

/// Report from cleaning up multiple worktrees.
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed: u64,
    pub overlays_unmounted: u64,
    pub btrfs_deleted: u64,
    pub errors: u64,
}

/// Remove all worktrees under a directory.
///
/// Scans the given directory for subdirectories (one or two levels deep to
/// handle `~/.echo-agent/worktrees/<repo>/<session>/`) and calls `remove_worktree()`
/// on each. Useful during session teardown to clean up all session worktrees.
///
/// This is a **blocking** operation.
pub fn cleanup_worktrees_in(dir: &std::path::Path) -> CleanupReport {
    cleanup_worktrees_in_with_delegate(dir, None)
}

/// Remove all worktrees under a directory, using an optional delegate for
/// privileged btrfs operations.
///
/// Like `cleanup_worktrees_in`, but forwards the delegate to each
/// `remove_worktree_with_delegate` call so that rootless hosts can clean up
/// btrfs snapshots via a privileged helper.
pub fn cleanup_worktrees_in_with_delegate(
    dir: &std::path::Path,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
) -> CleanupReport {
    let mut report = CleanupReport::default();

    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!(dir = %dir.display(), "cleanup: directory not readable");
        return report;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // symlink_metadata so a symlink-exposed worktree (btrfs snapshot layout),
        // including a now-dangling one, is handled: `is_dir()` follows the link
        // and returns false for a broken symlink, leaking it.
        let Ok(md) = path.symlink_metadata() else {
            continue;
        };
        if md.file_type().is_symlink() {
            cleanup_single_worktree(&path, delegate.as_ref(), &mut report);
            continue;
        }
        if !md.is_dir() {
            continue;
        }

        let has_git = path.join(".git").exists();

        if has_git {
            cleanup_single_worktree(&path, delegate.as_ref(), &mut report);
        } else {
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                for sub_entry in sub_entries.flatten() {
                    let sub_path = sub_entry.path();
                    if let Ok(sub_md) = sub_path.symlink_metadata()
                        && (sub_md.file_type().is_symlink() || sub_md.is_dir())
                    {
                        cleanup_single_worktree(&sub_path, delegate.as_ref(), &mut report);
                    }
                }
            }
            let _ = std::fs::remove_dir(&path);
        }
    }

    tracing::info!(
        dir = %dir.display(),
        removed = report.removed,
        overlays = report.overlays_unmounted,
        btrfs = report.btrfs_deleted,
        errors = report.errors,
        "worktree cleanup complete"
    );

    report
}

fn cleanup_single_worktree(
    path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
    report: &mut CleanupReport,
) {
    match remove_worktree_inner(path, delegate) {
        Ok(r) => {
            report.removed += 1;
            if r.unmounted_overlay {
                report.overlays_unmounted += 1;
            }
            if r.used_btrfs_delete {
                report.btrfs_deleted += 1;
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to clean up worktree"
            );
            report.errors += 1;
        }
    }
}

/// Scan known overlay roots under `/local/repo-fuse-*/worktrees/` for orphaned
/// overlay snapshots.
///
/// An overlay snapshot is orphaned if its metadata file exists but the
/// `mount_target` doesn't exist or isn't mounted. For each orphan: delete
/// the btrfs snapshot, remove the work dir, and clean up metadata.
///
/// Intended for host startup / periodic cleanup of leftovers from unclean
/// exits.
///
/// This is a **blocking** operation.
#[cfg(target_os = "linux")]
pub fn cleanup_orphaned_overlay_snapshots() -> CleanupReport {
    crate::overlay::cleanup_orphaned_overlay_snapshots()
}

/// Try to remove an overlay worktree.
/// Returns `Ok(Some(report))` if overlay was detected and removed, `Ok(None)` to fall back.
#[cfg(target_os = "linux")]
fn try_overlay_remove(
    worktree_path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    use crate::overlay;

    if let Some(report) = overlay::try_remove_from_mountinfo(worktree_path, delegate)? {
        return Ok(Some(report));
    }

    if let Some(report) = overlay::try_remove_from_metadata(worktree_path, delegate)? {
        return Ok(Some(report));
    }

    Ok(None)
}

/// Read the `gitdir:` pointer from a linked worktree's `.git` file.
///
/// Linked worktrees have `.git` as a plain file containing:
/// ```text
/// gitdir: /path/to/main-repo/.git/worktrees/<name>
/// ```
///
/// Returns the resolved path to the registration directory, or `None`
/// if the worktree doesn't have a `.git` file (standalone repo or missing).
fn read_worktree_gitdir(worktree_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let git_file = worktree_path.join(".git");
    let content = std::fs::read_to_string(&git_file).ok()?;
    let gitdir = content.trim().strip_prefix("gitdir: ")?;
    let path = std::path::Path::new(gitdir);
    let resolved = if path.is_relative() {
        worktree_path.join(path)
    } else {
        path.to_path_buf()
    };
    dunce::canonicalize(&resolved).ok().or(Some(resolved))
}

/// Delete `snapshot_path`, falling back to the delegate's `delete_snapshot`
/// (keyed by `worktree_path`) when the direct btrfs delete fails, e.g. EPERM on
/// a rootless host (no `CAP_SYS_ADMIN`) where only a privileged helper can run
/// `btrfs subvolume delete`.
///
/// `Some` means the delegate handled it; `None` means the direct delete succeeded
/// and the caller still owns local cleanup.
#[cfg(target_os = "linux")]
fn delete_snapshot_with_delegate_fallback(
    snapshot_path: &std::path::Path,
    worktree_path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
    delete: impl FnOnce(&std::path::Path) -> Result<()>,
) -> Result<Option<RemoveReport>> {
    let Err(e) = delete(snapshot_path) else {
        return Ok(None);
    };
    if let Some(delegate) = delegate {
        tracing::info!(
            path = %worktree_path.display(),
            "btrfs subvolume delete failed, trying delegate"
        );
        match delegate.delete_snapshot(worktree_path) {
            Ok(report) => return Ok(Some(report)),
            Err(delegate_err) => {
                tracing::warn!(error = %delegate_err, "delegate deletion also failed");
            }
        }
    }
    Err(e)
}

/// Try to remove a worktree using btrfs subvolume delete.
/// Returns `Ok(Some(report))` if btrfs was used, `Ok(None)` to fall back to git.
///
/// Handles three cases:
/// 1. **Symlinked worktree** (delegate path): `worktree_path` is a symlink to a
///    btrfs snapshot. Delete the snapshot, then remove the symlink.
/// 2. **Bind-mounted worktree**: `worktree_path` is a bind mount from a btrfs
///    snapshot. Unmount, then delete the snapshot subvolume.
/// 3. **Direct btrfs worktree**: `worktree_path` itself is the btrfs subvolume.
///    Delete it directly.
#[cfg(target_os = "linux")]
fn try_btrfs_remove(
    worktree_path: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    use crate::btrfs;
    use anyhow::Context;

    // Case 1: Symlink to a btrfs snapshot (created by the delegate path on
    // rootless hosts). Symlinks cross mount namespaces; this is the
    // counterpart to the privileged helper's symlink creation.
    if worktree_path.is_symlink() {
        let link_target = match std::fs::read_link(worktree_path) {
            Ok(t) => t,
            // Broken/unreadable symlink: unlink it so it isn't left dangling
            // (the `rm -rf` fallback follows the dead link and would miss it).
            Err(_) => {
                let _ = std::fs::remove_file(worktree_path);
                return Ok(None);
            }
        };

        let resolved = if link_target.is_relative() {
            worktree_path
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .join(&link_target)
        } else {
            link_target
        };

        if let Ok(Some(_)) = btrfs::is_btrfs_subvolume(&resolved) {
            // Refuse to follow a confused/planted symlink into deleting a
            // subvolume outside the snapshot storage (e.g. the live source repo).
            // The symlink itself is just a pointer, so removing it is always safe.
            if !btrfs::is_safe_snapshot_delete_target(&resolved) {
                tracing::warn!(
                    symlink = %worktree_path.display(),
                    target = %resolved.display(),
                    "refusing to delete subvolume outside snapshot storage; removing only the symlink"
                );
                let _ = std::fs::remove_file(worktree_path);
                return Ok(Some(RemoveReport {
                    used_btrfs_delete: false,
                    unmounted_bind: false,
                    unmounted_overlay: false,
                }));
            }

            tracing::info!(
                symlink = %worktree_path.display(),
                target = %resolved.display(),
                "removing symlinked btrfs worktree"
            );

            // Delete snapshot first: if this fails, the symlink still
            // references it so cleanup can be retried.
            //
            // Known residual TOCTOU: validation `lstat`s/canonicalizes then we
            // delete by path (the `btrfs subvolume delete` CLI takes a path, not
            // an fd, so there is no `unlinkat` to close the window). Bounded by:
            // `btrfs` refuses non-subvolumes, the snapshot dir is echo-agent-owned, and
            // `..`/symlink targets are already rejected. Accepted as-is.
            if let Some(report) = delete_snapshot_with_delegate_fallback(
                &resolved,
                worktree_path,
                delegate,
                btrfs::delete_snapshot,
            )? {
                return Ok(Some(report));
            }
            btrfs::remove_btrfs_metadata(&resolved);
            let _ = std::fs::remove_file(worktree_path);

            return Ok(Some(RemoveReport {
                used_btrfs_delete: true,
                unmounted_bind: false,
                unmounted_overlay: false,
            }));
        }

        // Symlink to non-btrfs target: remove it and fall through.
        let _ = std::fs::remove_file(worktree_path);
    }

    // Case 2 & 3: Check if the worktree path is a btrfs subvolume.
    let btrfs_info = match btrfs::is_btrfs_subvolume(worktree_path) {
        Ok(Some(info)) => info,
        Ok(None) => return Ok(None), // Not a btrfs subvolume, fall back
        Err(e) => {
            tracing::debug!(
                path = %worktree_path.display(),
                error = %e,
                "btrfs detection failed, falling back to git worktree remove"
            );
            return Ok(None);
        }
    };

    tracing::info!(
        path = %worktree_path.display(),
        bind_mount = ?btrfs_info.bind_mount_source,
        "removing worktree via btrfs subvolume delete (O(1))"
    );

    let mut unmounted_bind = false;

    // Case 2 (legacy bind mount): unmount first, then delete snapshot.
    if btrfs_info.bind_mount_source.is_some() {
        let mut umount_cmd = std::process::Command::new("umount");
        echo_agent_tty_utils::detach_std_command(&mut umount_cmd);
        umount_cmd.stdin(std::process::Stdio::null());
        let output = umount_cmd
            .arg(worktree_path)
            .output()
            .context("failed to execute umount")?;

        if output.status.success() {
            unmounted_bind = true;
            let _ = std::fs::remove_dir(worktree_path);
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                path = %worktree_path.display(),
                stderr = %stderr.trim(),
                "umount failed, attempting direct snapshot deletion"
            );
            // Don't return; proceed to delete the snapshot directly.
            // The mount point may be stale after an unclean host restart.
        }
    }

    let snapshot_path = btrfs_info
        .bind_mount_source
        .as_deref()
        .unwrap_or(worktree_path);

    // Reuse the hardened `btrfs::delete_snapshot` (OsStr args, no lossy
    // `.`-default) rather than re-spawning the command inline.
    if let Some(report) = delete_snapshot_with_delegate_fallback(
        snapshot_path,
        worktree_path,
        delegate,
        btrfs::delete_snapshot,
    )? {
        return Ok(Some(report));
    }

    btrfs::remove_btrfs_metadata(snapshot_path);

    tracing::info!(
        path = %worktree_path.display(),
        "btrfs subvolume deleted successfully"
    );

    Ok(Some(RemoveReport {
        used_btrfs_delete: true,
        unmounted_bind,
        unmounted_overlay: false,
    }))
}

/// Try to remove via persisted btrfs snapshot metadata (crash recovery).
///
/// Scans btrfs mount points for `*.btrfs-meta.json` files whose
/// `mount_target` matches `target`. Works even after the bind mount is gone.
#[cfg(target_os = "linux")]
fn try_btrfs_remove_from_metadata(
    target: &std::path::Path,
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    let mount_entries = match crate::mount_info::parse_mountinfo() {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    try_btrfs_remove_from_metadata_inner(target, &mount_entries, delegate)
}

#[cfg(target_os = "linux")]
fn try_btrfs_remove_from_metadata_inner(
    target: &std::path::Path,
    mount_entries: &[crate::mount_info::MountEntry],
    delegate: Option<&Arc<dyn BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    use crate::btrfs;

    for entry in mount_entries {
        if entry.fs_type != "btrfs" {
            continue;
        }

        for subdir in btrfs::BTRFS_SNAPSHOT_SUBDIRS {
            let dir = entry.mount_point.join(subdir);
            let Ok(dir_entries) = std::fs::read_dir(&dir) else {
                continue;
            };

            for dir_entry in dir_entries.flatten() {
                let name = dir_entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.ends_with(btrfs::BTRFS_META_SUFFIX) {
                    continue;
                }

                let meta_path = dir_entry.path();
                let Ok(content) = std::fs::read_to_string(&meta_path) else {
                    continue;
                };
                let Ok(meta) = serde_json::from_str::<btrfs::BtrfsSnapshotMetadata>(&content)
                else {
                    continue;
                };

                if meta.mount_target != target {
                    continue;
                }

                tracing::info!(
                    target = %target.display(),
                    snapshot = %meta.snapshot_path.display(),
                    "found btrfs snapshot metadata for worktree"
                );

                // `meta.snapshot_path` comes from an attacker-controllable
                // metadata file. Only delete it when it is a contained snapshot
                // subvolume located directly inside the directory we scanned.
                let snapshot_contained = meta.snapshot_path.parent() == Some(dir.as_path())
                    && btrfs::is_safe_snapshot_delete_target(&meta.snapshot_path);

                let target_is_symlink = target.is_symlink();
                let mut unmounted = false;

                // A legacy bind-mount directory must be unmounted before its
                // snapshot subvolume can be deleted; a symlink needs no umount.
                if !target_is_symlink {
                    let mut umount_cmd = std::process::Command::new("umount");
                    echo_agent_tty_utils::detach_std_command(&mut umount_cmd);
                    umount_cmd.stdin(std::process::Stdio::null());
                    if let Ok(output) = umount_cmd.arg(target).output() {
                        unmounted = output.status.success();
                    }
                }

                // Delete the snapshot BEFORE removing the worktree reference, so
                // the link/dir still points at it if deletion fails (retriable),
                // consistent with `try_btrfs_remove` Case 1.
                let mut deleted = false;
                let mut refused = false;
                if meta.snapshot_path.exists() {
                    if snapshot_contained {
                        if let Err(e) = btrfs::delete_snapshot(&meta.snapshot_path) {
                            // Try delegate fallback for sandboxed/rootless setups.
                            if let Some(delegate) = delegate {
                                tracing::info!(
                                    path = %meta.snapshot_path.display(),
                                    "btrfs delete failed in metadata path, trying delegate"
                                );
                                match delegate.delete_snapshot(target) {
                                    Ok(report) => return Ok(Some(report)),
                                    Err(delegate_err) => {
                                        tracing::warn!(
                                            error = %delegate_err,
                                            "delegate deletion also failed in metadata path"
                                        );
                                    }
                                }
                            }
                            return Err(e);
                        }
                        deleted = true;
                    } else {
                        refused = true;
                        tracing::warn!(
                            snapshot = %meta.snapshot_path.display(),
                            dir = %dir.display(),
                            "refusing to delete btrfs snapshot referenced by metadata: \
                             path is outside the scanned snapshot storage; preserving metadata"
                        );
                    }
                }

                // Remove the worktree reference (symlink file or empty dir). The
                // pointer is always safe to drop regardless of the refusal above.
                if target_is_symlink {
                    let _ = std::fs::remove_file(target);
                } else {
                    let _ = std::fs::remove_dir(target);
                }

                // Discard the metadata only when we handled the snapshot (deleted
                // it, or it was already gone). On refusal, keep it so the orphan
                // scanner can retry / it can be inspected.
                if !refused {
                    let _ = std::fs::remove_file(&meta_path);
                }

                return Ok(Some(RemoveReport {
                    used_btrfs_delete: deleted,
                    unmounted_bind: unmounted,
                    unmounted_overlay: false,
                }));
            }
        }
    }

    Ok(None)
}

/// Scan btrfs mount points for orphaned direct btrfs snapshots.
///
/// A btrfs snapshot is orphaned if its metadata file exists but the
/// `mount_target` is not an active mount point. For each orphan: unmount
/// stale target, delete the btrfs snapshot, and remove metadata.
///
/// This is the btrfs counterpart to `cleanup_orphaned_overlay_snapshots()`.
#[cfg(target_os = "linux")]
pub fn cleanup_orphaned_btrfs_snapshots() -> CleanupReport {
    let mount_entries = match crate::mount_info::parse_mountinfo() {
        Ok(e) => e,
        Err(_) => return CleanupReport::default(),
    };

    cleanup_orphaned_btrfs_snapshots_inner(&mount_entries)
}

/// Whether the symlink at `link` resolves to `target`.
///
/// Returns `false` when `link` is not a symlink or cannot be read. Used to
/// recognize a **live** symlink worktree (current layout) whose `mount_target`
/// never appears in mountinfo, so the orphan scanner does not destroy it.
#[cfg(target_os = "linux")]
fn symlink_resolves_to(link: &std::path::Path, target: &std::path::Path) -> bool {
    if !link.is_symlink() {
        return false;
    }
    match std::fs::read_link(link) {
        Ok(t) if t.is_relative() => {
            link.parent().unwrap_or(std::path::Path::new("/")).join(t) == target
        }
        Ok(t) => t == target,
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn cleanup_orphaned_btrfs_snapshots_inner(
    mount_entries: &[crate::mount_info::MountEntry],
) -> CleanupReport {
    use crate::btrfs;

    let mut report = CleanupReport::default();

    for mount_point in mount_entries
        .iter()
        .filter(|e| e.fs_type == "btrfs")
        .map(|e| &e.mount_point)
    {
        for subdir in btrfs::BTRFS_SNAPSHOT_SUBDIRS {
            let dir = mount_point.join(subdir);
            let Ok(dir_entries) = std::fs::read_dir(&dir) else {
                continue;
            };

            for dir_entry in dir_entries.flatten() {
                let name = dir_entry.file_name();
                let name_str = name.to_string_lossy();
                if !name_str.ends_with(btrfs::BTRFS_META_SUFFIX) {
                    continue;
                }

                let meta_path = dir_entry.path();
                let Ok(content) = std::fs::read_to_string(&meta_path) else {
                    let _ = std::fs::remove_file(&meta_path);
                    continue;
                };
                let Ok(meta) = serde_json::from_str::<btrfs::BtrfsSnapshotMetadata>(&content)
                else {
                    let _ = std::fs::remove_file(&meta_path);
                    continue;
                };

                // A live worktree is active either as a bind mount (appears in
                // mountinfo) or as a symlink resolving to its snapshot (the
                // current layout; `mount_target` is a symlink, never in
                // mountinfo). Both must be treated as active, not orphaned.
                let is_active = mount_entries
                    .iter()
                    .any(|e| e.mount_point == meta.mount_target)
                    || symlink_resolves_to(&meta.mount_target, &meta.snapshot_path);

                if is_active {
                    tracing::debug!(
                        snapshot = %meta.snapshot_path.display(),
                        target = %meta.mount_target.display(),
                        "skipping active btrfs snapshot"
                    );
                    continue;
                }

                // If the mount_target's parent dir is missing we cannot prove the snapshot
                // is orphaned: this scanner runs before restore recreates worktree dirs, so a
                // snapshot about to be re-exposed would be wrongly destroyed. Skipping at worst
                // leaks a true orphan (reclaimed on a later cycle), strictly safer than deleting.
                if let Some(parent) = meta.mount_target.parent()
                    && !parent.exists()
                {
                    tracing::debug!(
                        snapshot = %meta.snapshot_path.display(),
                        target = %meta.mount_target.display(),
                        "skipping btrfs snapshot: mount_target parent missing (cannot prove orphaned)"
                    );
                    continue;
                }

                // Untrusted metadata: only delete a snapshot contained directly
                // in the directory we scanned. Leave anything else (and its
                // metadata) untouched for inspection.
                let snapshot_contained = meta.snapshot_path.parent() == Some(dir.as_path())
                    && btrfs::is_safe_snapshot_delete_target(&meta.snapshot_path);
                if meta.snapshot_path.exists() && !snapshot_contained {
                    tracing::warn!(
                        snapshot = %meta.snapshot_path.display(),
                        dir = %dir.display(),
                        "refusing to delete btrfs snapshot outside scanned storage"
                    );
                    report.errors += 1;
                    continue;
                }

                tracing::info!(
                    target = %meta.mount_target.display(),
                    snapshot = %meta.snapshot_path.display(),
                    "cleaning up orphaned btrfs snapshot"
                );

                if meta.mount_target.is_symlink() {
                    let _ = std::fs::remove_file(&meta.mount_target);
                } else {
                    let mut umount_cmd = std::process::Command::new("umount");
                    echo_agent_tty_utils::detach_std_command(&mut umount_cmd);
                    umount_cmd.stdin(std::process::Stdio::null());
                    let _ = umount_cmd.arg(&meta.mount_target).output();
                    let _ = std::fs::remove_dir(&meta.mount_target);
                }

                if meta.snapshot_path.exists() {
                    if let Err(e) = btrfs::delete_snapshot(&meta.snapshot_path) {
                        tracing::warn!(
                            path = %meta.snapshot_path.display(),
                            error = %e,
                            "failed to delete orphaned btrfs snapshot"
                        );
                        report.errors += 1;
                        // Preserve metadata so the orphan scanner can retry
                        // on the next cycle instead of losing track of it.
                        continue;
                    } else {
                        report.btrfs_deleted += 1;
                    }
                }

                let _ = std::fs::remove_file(&meta_path);
                report.removed += 1;
            }
        }
    }

    if report.removed > 0 || report.errors > 0 {
        tracing::info!(
            removed = report.removed,
            btrfs = report.btrfs_deleted,
            errors = report.errors,
            "orphaned btrfs snapshot cleanup complete"
        );
    }

    report
}

#[cfg(feature = "metadata")]
pub(crate) fn register_worktree(
    worktree_path: &std::path::Path,
    source: &std::path::Path,
    kind: crate::db::WorktreeKind,
    creation_mode: &str,
    git_ref: &str,
    commit: &str,
    session_id: Option<String>,
    worktree_id: Option<String>,
    metadata: Option<serde_json::Value>,
) {
    use crate::db;

    let db = match db::WorktreeDb::open_default() {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "failed to open worktree DB for registration");
            return;
        }
    };
    // Same canonical path as discovery rebuild / WorktreeDb::get so macOS
    // /var vs /private/var (and other symlink roots) do not create duplicate rows.
    let path = dunce::canonicalize(worktree_path).unwrap_or_else(|_| worktree_path.to_path_buf());
    let source = dunce::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let record = db::WorktreeRecord {
        id: worktree_id.unwrap_or_else(|| db::id_from_path(&path)),
        path,
        source_repo: source.clone(),
        repo_name: db::repo_name_from_path(&source),
        kind,
        creation_mode: creation_mode.to_owned(),
        git_ref: Some(git_ref.to_owned()),
        head_commit: Some(commit.to_owned()),
        session_id,
        creator_pid: Some(std::process::id()),
        created_at: db::now_epoch_secs(),
        last_accessed_at: None,
        status: db::WorktreeStatus::Alive,
        metadata,
    };
    if let Err(e) = db.register(&record) {
        tracing::warn!(error = %e, "failed to register worktree in DB");
    }
}

#[cfg(feature = "metadata")]
fn unregister_worktree(worktree_path: &std::path::Path) {
    if let Ok(db) = crate::db::WorktreeDb::open_default() {
        let path =
            dunce::canonicalize(worktree_path).unwrap_or_else(|_| worktree_path.to_path_buf());
        let _ = db.unregister_by_path(&path);
    }
}

/// Test-only `BtrfsDelegate` that returns a fixed snapshot and counts
/// `delete_snapshot` calls. Shared by the delegate-arm reclaim tests
/// (`worktree::execute`) and the gc-with-delegate tests.
#[cfg(test)]
pub(crate) struct RecordingDelegate {
    pub snapshot_path: PathBuf,
    pub worktree_path: PathBuf,
    pub deletes: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl BtrfsDelegate for RecordingDelegate {
    fn create_snapshot(&self, _source: &Path, _dest: &Path) -> Result<DelegateSnapshotResult> {
        Ok(DelegateSnapshotResult {
            snapshot_path: self.snapshot_path.clone(),
            worktree_path: self.worktree_path.clone(),
            bind_mounted: false,
        })
    }

    fn delete_snapshot(&self, _worktree_path: &Path) -> Result<RemoveReport> {
        self.deletes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(RemoveReport {
            used_btrfs_delete: true,
            unmounted_bind: false,
            unmounted_overlay: false,
        })
    }
}

#[cfg(feature = "metadata")]
pub mod gc;

#[cfg(all(test, feature = "metadata"))]
#[path = "api/gc/integration_tests.rs"]
mod gc_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_out_of_disk_detects_storage_full_kind() {
        // Cross-platform: std maps ENOSPC and the Windows disk-full codes onto
        // ErrorKind::StorageFull, so the typed check fires on every OS.
        let io = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let err = anyhow::Error::new(io).context("failed to copy index from a to b");
        assert!(is_out_of_disk(&err));
    }

    #[cfg(unix)]
    #[test]
    fn is_out_of_disk_detects_enospc_io_error() {
        // Real ENOSPC (errno 28 on Linux/macOS) must decode to StorageFull.
        let io = std::io::Error::from_raw_os_error(28);
        assert_eq!(io.kind(), std::io::ErrorKind::StorageFull);
        let err = anyhow::Error::new(io).context("failed to copy index from a to b");
        assert!(is_out_of_disk(&err));
    }

    #[cfg(windows)]
    #[test]
    fn is_out_of_disk_detects_windows_disk_full_codes() {
        // Windows reports a full disk as ERROR_DISK_FULL (112) or
        // ERROR_HANDLE_DISK_FULL (39); std decodes both to StorageFull.
        for code in [112, 39] {
            let io = std::io::Error::from_raw_os_error(code);
            assert_eq!(io.kind(), std::io::ErrorKind::StorageFull);
            let err = anyhow::Error::new(io).context("failed to copy index from a to b");
            assert!(is_out_of_disk(&err));
        }
    }

    #[test]
    fn is_out_of_disk_detects_message_text() {
        // `git` subcommands surface ENOSPC only as stderr text.
        let err = anyhow::anyhow!("git worktree add failed: No space left on device");
        assert!(is_out_of_disk(&err));
    }

    #[test]
    fn is_out_of_disk_ignores_unrelated_errors() {
        let err = anyhow::anyhow!("failed to get HEAD commit from source");
        assert!(!is_out_of_disk(&err));
    }

    #[test]
    fn annotate_disk_full_promotes_reason_to_top_context() {
        let err = anyhow::anyhow!("failed to copy index: No space left on device (os error 28)");
        let annotated = annotate_disk_full(err);
        // Display (top context only) now carries the disk reason, so it
        // survives the workspace/ACP flattening to a single message.
        assert_eq!(annotated.to_string(), OUT_OF_DISK_CONTEXT);
        // The original chain is preserved underneath for logs.
        assert!(format!("{annotated:#}").contains("failed to copy index"));
    }

    #[test]
    fn annotate_disk_full_leaves_other_errors_unchanged() {
        let err = anyhow::anyhow!("some other failure");
        assert_eq!(annotate_disk_full(err).to_string(), "some other failure");
    }

    #[test]
    fn test_copy_report_from_copy_stats() {
        let stats = CopyStats {
            files_copied: 10,
            dirs_created: 3,
            symlinks_copied: 2,
            files_skipped: 5,
            issues: vec!["warning 1".to_string(), "warning 2".to_string()],
        };

        let report: CopyReport = stats.into();
        assert_eq!(report.files_copied, 10);
        assert_eq!(report.dirs_created, 3);
        assert_eq!(report.symlinks_copied, 2);
        assert_eq!(report.files_skipped, 5);
        assert_eq!(report.issues.len(), 2);
        assert!(report.dirty_files.is_none());
    }

    #[test]
    fn test_btrfs_mode_default() {
        let mode = BtrfsMode::default();
        assert_eq!(mode, BtrfsMode::Auto);
    }

    #[test]
    fn test_btrfs_mode_variants() {
        assert_eq!(BtrfsMode::Auto, BtrfsMode::Auto);
        assert_eq!(BtrfsMode::Force, BtrfsMode::Force);
        assert_eq!(BtrfsMode::Disabled, BtrfsMode::Disabled);

        assert_ne!(BtrfsMode::Auto, BtrfsMode::Force);
        assert_ne!(BtrfsMode::Auto, BtrfsMode::Disabled);
        assert_ne!(BtrfsMode::Force, BtrfsMode::Disabled);
    }

    #[test]
    fn test_btrfs_mode_debug() {
        let auto = format!("{:?}", BtrfsMode::Auto);
        let force = format!("{:?}", BtrfsMode::Force);
        let disabled = format!("{:?}", BtrfsMode::Disabled);

        assert!(auto.contains("Auto"));
        assert!(force.contains("Force"));
        assert!(disabled.contains("Disabled"));
    }

    #[test]
    fn test_btrfs_mode_clone() {
        let mode = BtrfsMode::Force;
        let cloned = mode.clone();
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_creation_mode_default() {
        let mode = CreationMode::default();
        assert_eq!(mode, CreationMode::Linked);
    }

    #[test]
    fn test_creation_mode_variants() {
        assert_eq!(CreationMode::Linked, CreationMode::Linked);
        assert_eq!(CreationMode::Standalone, CreationMode::Standalone);
        assert_eq!(CreationMode::GitCheckout, CreationMode::GitCheckout);
        assert_ne!(CreationMode::Linked, CreationMode::Standalone);
        assert_ne!(CreationMode::Linked, CreationMode::GitCheckout);
    }

    #[test]
    fn test_worktree_builder_chain() {
        let _builder = WorktreeBuilder::new("/source", "/dest")
            .git_ref("main")
            .parallelism(4)
            .ignored_parallelism(2)
            .channel_buffer(512)
            .working_tree_mode(WorkingTreeMode::CleanAll)
            .ignored_files_mode(IgnoredFilesMode::Copy {
                skip_patterns: vec!["*.log".to_string()],
            })
            .creation_mode(CreationMode::GitCheckout);
    }

    #[test]
    fn test_standalone_shorthand() {
        let _builder = WorktreeBuilder::new("/source", "/dest").standalone(true);
    }

    #[test]
    fn copy_ignored_only_returns_err_when_cancelled() {
        let src = tempfile::TempDir::new().unwrap();
        let dest = tempfile::TempDir::new().unwrap();
        std::fs::write(src.path().join("file.txt"), "content").unwrap();

        let token = CancellationToken::new();
        token.cancel();

        let err = WorktreeBuilder::new(src.path(), dest.path())
            .cancellation_token(token)
            .copy_ignored_only()
            .expect_err("a pre-cancelled token must produce an error, not Ok(partial)");

        assert!(
            err.to_string().contains("cancelled"),
            "error should report cancellation, got: {err}"
        );
    }

    #[test]
    fn test_cleanup_report_default() {
        let report = CleanupReport::default();
        assert_eq!(report.removed, 0);
        assert_eq!(report.overlays_unmounted, 0);
        assert_eq!(report.btrfs_deleted, 0);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn test_cleanup_worktrees_in_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = cleanup_worktrees_in(tmp.path());
        assert_eq!(report.removed, 0);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn test_cleanup_worktrees_in_missing_dir() {
        let report = cleanup_worktrees_in(std::path::Path::new("/nonexistent/path/xyz"));
        assert_eq!(report.removed, 0);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn test_cleanup_worktrees_in_with_plain_worktrees() {
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};

        let tmp = tempfile::TempDir::new().unwrap();

        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let wt1 = worktrees_dir.join("wt1");
        let wt2 = worktrees_dir.join("wt2");

        WorktreeBuilder::new(&repo_path, &wt1).create().unwrap();
        WorktreeBuilder::new(&repo_path, &wt2).create().unwrap();

        assert!(wt1.exists());
        assert!(wt2.exists());

        let report = cleanup_worktrees_in(&worktrees_dir);
        assert_eq!(report.removed, 2);
        assert_eq!(report.errors, 0);
        assert!(!wt1.exists());
        assert!(!wt2.exists());
    }

    #[test]
    fn test_cleanup_worktrees_in_with_nested_dirs() {
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};

        let tmp = tempfile::TempDir::new().unwrap();

        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "content").unwrap();
        git_commit_all(&repo_path, "initial");

        let worktrees_dir = tmp.path().join("worktrees");
        let repo_group = worktrees_dir.join("myrepo");
        std::fs::create_dir_all(&repo_group).unwrap();

        let wt1 = repo_group.join("session-1");
        WorktreeBuilder::new(&repo_path, &wt1).create().unwrap();
        assert!(wt1.exists());

        let report = cleanup_worktrees_in(&worktrees_dir);
        assert_eq!(report.removed, 1);
        assert_eq!(report.errors, 0);
        assert!(!wt1.exists());
        // Grouping dir should be removed since it's empty now.
        assert!(!repo_group.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_cleanup_worktrees_in_removes_dangling_symlink() {
        // A worktree exposed as a symlink whose snapshot was already deleted is a
        // dangling symlink; it must be unlinked, not skipped (`is_dir()` follows
        // the link and returns false, which would leak it).
        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let dangling = worktrees_dir.join("dead-wt");
        std::os::unix::fs::symlink(tmp.path().join("gone-snapshot"), &dangling).unwrap();
        assert!(dangling.symlink_metadata().is_ok());
        assert!(!dangling.is_dir(), "precondition: dangling symlink");

        let report = cleanup_worktrees_in(&worktrees_dir);

        assert!(
            dangling.symlink_metadata().is_err(),
            "dangling symlink worktree must be removed, not skipped"
        );
        assert_eq!(report.removed, 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_cleanup_worktrees_in_removes_nested_dangling_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        // A grouping dir with NO `.git`, so cleanup recurses into it.
        let repo_group = worktrees_dir.join("myrepo");
        std::fs::create_dir_all(&repo_group).unwrap();

        let dangling = repo_group.join("dead-session");
        std::os::unix::fs::symlink(tmp.path().join("gone-snapshot"), &dangling).unwrap();
        assert!(!dangling.is_dir(), "precondition: dangling symlink");

        let report = cleanup_worktrees_in(&worktrees_dir);

        assert!(
            dangling.symlink_metadata().is_err(),
            "nested dangling symlink worktree must be removed"
        );
        assert_eq!(report.removed, 1);
    }

    #[test]
    fn test_remove_report_has_overlay_field() {
        let report = RemoveReport {
            used_btrfs_delete: false,
            unmounted_bind: false,
            unmounted_overlay: true,
        };
        assert!(report.unmounted_overlay);
        assert!(!report.used_btrfs_delete);
    }

    #[test]
    fn test_creation_mode_as_db_str() {
        assert_eq!(CreationMode::Linked.as_db_str(), "linked");
        assert_eq!(CreationMode::Standalone.as_db_str(), "standalone");
        assert_eq!(CreationMode::GitCheckout.as_db_str(), "git");
    }

    #[test]
    fn test_remove_worktree_with_delegate_no_delegate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent");
        let result = remove_worktree_with_delegate(&path, None);
        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(!report.used_btrfs_delete);
        assert!(!report.unmounted_bind);
        assert!(!report.unmounted_overlay);
    }

    #[test]
    fn test_remove_worktree_with_delegate_existing_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("some-dir");
        std::fs::create_dir(&path).unwrap();
        let result = remove_worktree_with_delegate(&path, None);
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    /// A plain (non-snapshot) linked worktree removed through the delegate-aware
    /// path must still deregister `.git/worktrees/<name>`, and the delegate must
    /// be used only as a fallback, never invoked when the direct removal succeeds.
    #[test]
    fn remove_with_delegate_deregisters_plain_worktree_without_calling_delegate() {
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
        // Isolate ECHO_AGENT_HOME so the post-removal unregister writes to a private DB.
        #[cfg(feature = "metadata")]
        let _fx = crate::db::EchoAgentHomeFixture::new();

        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("file.txt"), "content").unwrap();
        git_commit_all(&repo, "initial");

        let wt = tmp.path().join("worktrees").join("wt1");
        WorktreeBuilder::new(&repo, &wt).create().unwrap();

        let registration_dir =
            read_worktree_gitdir(&wt).expect("linked worktree must have a gitdir pointer");
        assert!(
            registration_dir.exists(),
            "precondition: registration exists"
        );

        let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delegate: Arc<dyn BtrfsDelegate> = Arc::new(RecordingDelegate {
            snapshot_path: PathBuf::from("/unused"),
            worktree_path: PathBuf::from("/unused"),
            deletes: deletes.clone(),
        });

        let report = remove_worktree_with_delegate(&wt, Some(delegate)).unwrap();

        assert!(!wt.exists(), "worktree directory must be removed");
        assert!(
            !registration_dir.exists(),
            "`.git/worktrees/<name>` registration must be deregistered"
        );
        assert!(!report.used_btrfs_delete);
        assert_eq!(
            deletes.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "delegate is a fallback only; a plain worktree removal must not call it"
        );
    }

    #[test]
    fn sibling_registration_not_removed() {
        echo_agent_test_utils::require_git!();
        use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
        #[cfg(feature = "metadata")]
        let _fx = crate::db::EchoAgentHomeFixture::new();

        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("file.txt"), "content").unwrap();
        git_commit_all(&repo, "initial");

        let victim_wt = tmp.path().join("worktrees").join("victim");
        let attacker_wt = tmp.path().join("worktrees").join("attacker");
        WorktreeBuilder::new(&repo, &victim_wt).create().unwrap();
        WorktreeBuilder::new(&repo, &attacker_wt).create().unwrap();

        let victim_reg = read_worktree_gitdir(&victim_wt).expect("victim has a registration");
        assert!(
            victim_reg.exists(),
            "precondition: victim registration exists"
        );
        assert_eq!(
            victim_reg.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new("worktrees")),
            "precondition: the sibling registration's parent is `worktrees`"
        );

        // Point the attacker worktree's `.git` at the victim's registration.
        std::fs::write(
            attacker_wt.join(".git"),
            format!("gitdir: {}\n", victim_reg.display()),
        )
        .unwrap();

        remove_worktree(&attacker_wt).unwrap();

        assert!(
            !attacker_wt.exists(),
            "the removed worktree is still deleted"
        );
        assert!(
            victim_reg.exists(),
            "a sibling registration must survive: its backlink resolves to the victim, not the removed worktree"
        );
        assert!(
            victim_reg.join("gitdir").exists(),
            "the sibling's refs and reflogs are left intact"
        );
    }

    #[test]
    fn non_registration_directory_not_removed() {
        #[cfg(feature = "metadata")]
        let _fx = crate::db::EchoAgentHomeFixture::new();

        let tmp = tempfile::TempDir::new().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        // Backlinks to `wt` but is not under `worktrees/`: passes backlink, fails shape.
        let decoy = tmp.path().join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(
            decoy.join("gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .unwrap();
        std::fs::write(decoy.join("keep.txt"), b"do not delete").unwrap();

        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", decoy.display())).unwrap();

        remove_worktree(&wt).unwrap();

        assert!(
            !wt.exists(),
            "the worktree directory itself is still removed"
        );
        assert!(
            decoy.join("keep.txt").exists(),
            "a directory that is not a worktrees entry must not be removed"
        );
    }

    /// When the direct `btrfs delete` fails (e.g. EPERM on a rootless host),
    /// the snapshot delete must fall back to the delegate and return its report.
    #[cfg(target_os = "linux")]
    #[test]
    fn delete_fallback_invokes_delegate_when_direct_delete_fails() {
        let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delegate: Arc<dyn BtrfsDelegate> = Arc::new(RecordingDelegate {
            snapshot_path: PathBuf::from("/unused"),
            worktree_path: PathBuf::from("/unused"),
            deletes: deletes.clone(),
        });

        let report = delete_snapshot_with_delegate_fallback(
            Path::new("/mnt/btrfs/worktrees/snap-1"),
            Path::new("/home/u/.echo-agent/worktrees/repo/wt"),
            Some(&delegate),
            |_| anyhow::bail!("operation not permitted (os error 1)"),
        )
        .unwrap()
        .expect("delegate fallback must handle the failed direct delete");
        assert!(report.used_btrfs_delete);
        assert_eq!(deletes.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// A successful direct delete returns `None` (caller does local cleanup) and
    /// never touches the delegate.
    #[cfg(target_os = "linux")]
    #[test]
    fn delete_fallback_skips_delegate_when_direct_delete_succeeds() {
        let deletes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delegate: Arc<dyn BtrfsDelegate> = Arc::new(RecordingDelegate {
            snapshot_path: PathBuf::from("/unused"),
            worktree_path: PathBuf::from("/unused"),
            deletes: deletes.clone(),
        });

        let res = delete_snapshot_with_delegate_fallback(
            Path::new("/snap"),
            Path::new("/wt"),
            Some(&delegate),
            |_| Ok(()),
        )
        .unwrap();
        assert!(res.is_none(), "successful direct delete must return None");
        assert_eq!(deletes.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// With no delegate, a failed direct delete propagates the original error so
    /// the worktree reference is preserved for a retry.
    #[cfg(target_os = "linux")]
    #[test]
    fn delete_fallback_without_delegate_propagates_error() {
        let err = delete_snapshot_with_delegate_fallback(
            Path::new("/snap"),
            Path::new("/wt"),
            None,
            |_| anyhow::bail!("EPERM marker"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("EPERM marker"));
    }

    /// Metadata persisted by the public `write_btrfs_metadata` lets metadata-based
    /// removal locate a worktree purely from its `mount_target` and drop the
    /// symlink + metadata, even when the snapshot subvolume is already gone.
    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_written_by_public_writer_is_found_by_metadata_removal() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let mount = tmp.path();
        let worktrees_dir = mount.join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let snapshot_path = worktrees_dir.join("snap-1");
        let dest = tmp.path().join("dest-worktree");
        std::os::unix::fs::symlink(&snapshot_path, &dest).unwrap();

        btrfs::write_btrfs_metadata(&snapshot_path, &dest).unwrap();
        let meta_path = btrfs::btrfs_meta_path(&snapshot_path).unwrap();
        assert!(
            meta_path.exists(),
            "metadata must be written next to snapshot"
        );
        assert!(
            dest.symlink_metadata().is_ok(),
            "precondition: symlink exists"
        );

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: mount.to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = try_btrfs_remove_from_metadata_inner(&dest, &entries, None)
            .unwrap()
            .expect("metadata removal must find the snapshot by mount_target");
        // Snapshot subvolume is already gone, so no btrfs delete is attempted.
        assert!(!report.used_btrfs_delete);
        assert!(
            dest.symlink_metadata().is_err(),
            "the worktree symlink must be removed"
        );
        assert!(
            !meta_path.exists(),
            "metadata must be cleaned up after handling"
        );
    }

    /// Metadata written by the public `write_btrfs_metadata` for a snapshot whose
    /// worktree is gone (orphan) must be discovered and reclaimed by the orphan
    /// scanner.
    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_written_by_public_writer_is_reclaimed_by_orphan_scan() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let mount = tmp.path();
        let worktrees_dir = mount.join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        // Orphan: the worktree's mount_target no longer exists (symlink lost),
        // so the scanner must treat it as reclaimable rather than active.
        let snapshot_path = worktrees_dir.join("snap-orphan");
        let mount_target = tmp.path().join("gone-dest");
        btrfs::write_btrfs_metadata(&snapshot_path, &mount_target).unwrap();
        let meta_path = btrfs::btrfs_meta_path(&snapshot_path).unwrap();
        assert!(meta_path.exists());

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: mount.to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = cleanup_orphaned_btrfs_snapshots_inner(&entries);
        assert_eq!(report.removed, 1, "orphaned snapshot must be reclaimed");
        // Snapshot subvolume doesn't exist on disk, so no btrfs delete is attempted.
        assert_eq!(report.btrfs_deleted, 0);
        assert!(!meta_path.exists(), "orphan metadata must be cleaned up");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_cleanup_orphaned_btrfs_snapshots_no_mounts() {
        let report = cleanup_orphaned_btrfs_snapshots_inner(&[]);
        assert_eq!(report.removed, 0);
        assert_eq!(report.errors, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_cleanup_orphaned_btrfs_snapshots_with_metadata() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        // Orphan: the mount_target is gone but its PARENT dir exists (home is
        // restored), so the scanner can prove it's orphaned and reclaim it.
        let mount_parent = tmp.path().join("home-restored");
        std::fs::create_dir(&mount_parent).unwrap();
        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: worktrees_dir.join("wt-abc"),
            mount_target: mount_parent.join("gone-target"),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("wt-abc.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = cleanup_orphaned_btrfs_snapshots_inner(&entries);
        assert_eq!(report.removed, 1);
        // snapshot_path doesn't exist as dir, so no btrfs delete attempted
        assert_eq!(report.btrfs_deleted, 0);
        assert!(!meta_path.exists(), "metadata should be cleaned up");
    }

    /// A snapshot whose `mount_target` parent dir is missing can't be proven orphaned,
    /// so the scanner must skip it rather than destroy one about to be re-exposed.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_cleanup_orphaned_btrfs_skips_when_mount_target_parent_missing() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        // mount_target lives under a home dir not yet restored. Hermetic: the parent
        // is a path inside this tempdir that the test never creates.
        let snapshot_path = worktrees_dir.join("wt-live");
        std::fs::create_dir(&snapshot_path).unwrap();
        let unrestored_home = tmp.path().join("unrestored-home");
        let mount_target = unrestored_home.join(".echo-agent/worktrees/x/wt-live");
        assert!(
            !mount_target.parent().unwrap().exists(),
            "precondition: mount_target parent must be absent"
        );
        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: snapshot_path.clone(),
            mount_target,
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("wt-live.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = cleanup_orphaned_btrfs_snapshots_inner(&entries);
        assert_eq!(
            report.removed, 0,
            "must not reclaim while orphan status is unprovable"
        );
        // The guard must skip cleanly: without it, a non-btrfs tempdir host would
        // instead error-class this as "outside scanned storage" (errors == 1).
        assert_eq!(report.errors, 0, "guard must skip cleanly, not error-class");
        assert!(
            meta_path.exists(),
            "metadata must be preserved for a later scan"
        );
        // The guard `continue`s before any delete, so the snapshot dir is untouched.
        assert!(snapshot_path.exists(), "snapshot must not be deleted");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_cleanup_orphaned_btrfs_skips_active() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let mount_target = std::path::PathBuf::from("/home/user/.echo-agent/worktrees/active-wt");

        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: worktrees_dir.join("active-wt"),
            mount_target: mount_target.clone(),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("active-wt.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        // mount_target appears in the mount entries (simulates active bind mount)
        let entries = vec![
            MountEntry {
                mount_id: 1,
                parent_id: 0,
                root: "/".to_string(),
                mount_point: tmp.path().to_path_buf(),
                fs_type: "btrfs".to_string(),
                source: "/dev/loop0".to_string(),
                super_options: String::new(),
            },
            MountEntry {
                mount_id: 2,
                parent_id: 1,
                root: "/worktrees/active-wt".to_string(),
                mount_point: mount_target,
                fs_type: "btrfs".to_string(),
                source: "/dev/loop0".to_string(),
                super_options: String::new(),
            },
        ];

        let report = cleanup_orphaned_btrfs_snapshots_inner(&entries);
        assert_eq!(report.removed, 0, "active snapshot should not be removed");
        assert!(
            meta_path.exists(),
            "metadata for active snapshot should remain"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_cleanup_orphaned_btrfs_skips_active_symlink() {
        // Current layout: the live worktree is a SYMLINK to the snapshot and
        // never appears in mountinfo. The orphan scanner must recognize it as
        // active (resolves to snapshot_path) and NOT delete it.
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        // The snapshot dir (a plain dir here) and a live symlink pointing at it.
        let snapshot_path = worktrees_dir.join("live-wt");
        std::fs::create_dir(&snapshot_path).unwrap();
        let mount_target = tmp.path().join("worktree-symlink");
        std::os::unix::fs::symlink(&snapshot_path, &mount_target).unwrap();

        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: snapshot_path.clone(),
            mount_target: mount_target.clone(),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("live-wt.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        // No mount entry references the symlink, only the btrfs mount itself.
        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = cleanup_orphaned_btrfs_snapshots_inner(&entries);
        assert_eq!(report.removed, 0, "active symlink worktree must be kept");
        assert!(
            meta_path.exists(),
            "metadata for live worktree should remain"
        );
        assert!(snapshot_path.exists(), "snapshot must not be deleted");
        assert!(
            mount_target.symlink_metadata().is_ok(),
            "live symlink must not be removed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_symlink_resolves_to() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("worktrees").join("snap");
        std::fs::create_dir_all(&target).unwrap();

        // Absolute-target symlink resolving to `target` → true.
        let abs_link = tmp.path().join("abs-link");
        std::os::unix::fs::symlink(&target, &abs_link).unwrap();
        assert!(symlink_resolves_to(&abs_link, &target));

        // Relative-target symlink resolving (via link.parent()) to `target` → true.
        let rel_link = tmp.path().join("rel-link");
        std::os::unix::fs::symlink(std::path::Path::new("worktrees/snap"), &rel_link).unwrap();
        assert!(symlink_resolves_to(&rel_link, &target));

        // Symlink resolving elsewhere → false.
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let wrong_link = tmp.path().join("wrong-link");
        std::os::unix::fs::symlink(&other, &wrong_link).unwrap();
        assert!(!symlink_resolves_to(&wrong_link, &target));

        // Non-symlink path → false.
        assert!(!symlink_resolves_to(&target, &target));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_btrfs_remove_from_metadata_finds_match() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let mount_target = tmp.path().join("mount-target");
        std::fs::create_dir(&mount_target).unwrap();

        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: worktrees_dir.join("snap-abc"),
            mount_target: mount_target.clone(),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("snap-abc.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        // `snapshot_path` is intentionally never created, so the privileged
        // `btrfs subvolume delete` is gated out (btrfs is unavailable in CI; real
        // subvolume deletion is exercised only on a btrfs-capable host). The
        // discriminating signals here are the metadata + dir cleanup.
        let report = try_btrfs_remove_from_metadata_inner(&mount_target, &entries, None)
            .unwrap()
            .expect("should find metadata match");
        // Nothing was deleted (snapshot absent) and this dir branch unmounts
        // nothing on an already-unmounted dir.
        assert!(!report.used_btrfs_delete);
        assert!(!report.unmounted_bind);
        assert!(!meta_path.exists(), "metadata should be cleaned up");
        assert!(!mount_target.exists(), "dir worktree should be removed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_btrfs_remove_from_metadata_removes_symlink_target() {
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        // The on-disk snapshot dir (a plain dir here, no real btrfs subvolume,
        // so deletion is skipped, but the symlink + metadata must be cleaned up).
        let snapshot_path = worktrees_dir.join("snap-link");

        let mount_target = tmp.path().join("worktree-symlink");
        std::os::unix::fs::symlink(&snapshot_path, &mount_target).unwrap();
        assert!(mount_target.is_symlink());

        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: snapshot_path.clone(),
            mount_target: mount_target.clone(),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("snap-link.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        // NOTE: `snapshot_path` is intentionally never created here, so the
        // privileged `btrfs subvolume delete` is gated out (btrfs is unavailable
        // in CI). This test covers the symlink-vs-dir branch selection and the
        // symlink + metadata cleanup; the real subvolume deletion is exercised
        // only on a btrfs-capable host.
        let result = try_btrfs_remove_from_metadata_inner(&mount_target, &entries, None);
        assert!(result.is_ok());
        let report = result.unwrap().expect("should find metadata match");
        // The symlink branch never unmounts a bind mount.
        assert!(!report.unmounted_bind);
        assert!(
            mount_target.symlink_metadata().is_err(),
            "symlink worktree should be removed"
        );
        assert!(!meta_path.exists(), "metadata should be cleaned up");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_btrfs_remove_from_metadata_removes_legacy_dir_target() {
        // Legacy bind-mount layout: `mount_target` is a real (empty) directory.
        // Exercises the non-symlink `else` branch (umount is a no-op on an
        // already-unmounted empty dir, then `remove_dir`).
        use crate::btrfs;
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let mount_target = tmp.path().join("mount-target");
        std::fs::create_dir(&mount_target).unwrap();
        assert!(!mount_target.is_symlink());

        let meta = btrfs::BtrfsSnapshotMetadata {
            kind: std::borrow::Cow::Borrowed("btrfs"),
            snapshot_path: worktrees_dir.join("snap-dir"),
            mount_target: mount_target.clone(),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let meta_path = worktrees_dir.join("snap-dir.btrfs-meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let report = try_btrfs_remove_from_metadata_inner(&mount_target, &entries, None)
            .unwrap()
            .expect("should find metadata match");
        assert!(!mount_target.exists(), "dir worktree should be removed");
        assert!(!meta_path.exists(), "metadata should be cleaned up");
        let _ = report;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_btrfs_remove_symlink_to_non_btrfs_target() {
        // A symlink whose target is not a btrfs subvolume: try_btrfs_remove
        // should remove the symlink and fall through (Ok(None) overall once the
        // now-removed path is no longer a btrfs subvolume).
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("plain-target");
        std::fs::create_dir(&target).unwrap();

        let link = tmp.path().join("worktree-symlink");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(link.is_symlink());

        let result = try_btrfs_remove(&link, None);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "should fall through for non-btrfs"
        );
        assert!(
            link.symlink_metadata().is_err(),
            "non-btrfs symlink should be removed before falling through"
        );
        // The target itself is untouched by the symlink removal.
        assert!(target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_btrfs_remove_from_metadata_no_match() {
        use crate::mount_info::MountEntry;

        let tmp = tempfile::TempDir::new().unwrap();
        let worktrees_dir = tmp.path().join("worktrees");
        std::fs::create_dir(&worktrees_dir).unwrap();

        let entries = vec![MountEntry {
            mount_id: 1,
            parent_id: 0,
            root: "/".to_string(),
            mount_point: tmp.path().to_path_buf(),
            fs_type: "btrfs".to_string(),
            source: "/dev/loop0".to_string(),
            super_options: String::new(),
        }];

        let result = try_btrfs_remove_from_metadata_inner(
            std::path::Path::new("/nonexistent/target"),
            &entries,
            None,
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
