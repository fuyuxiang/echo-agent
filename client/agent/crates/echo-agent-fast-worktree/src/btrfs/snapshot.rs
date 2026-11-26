//! BTRFS snapshot creation.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::detect::BtrfsInfo;
use crate::copy::shard::short_path_hash;

/// Create a writable BTRFS snapshot from source to dest.
///
/// The source must be a BTRFS subvolume. The dest path must not exist
/// but its parent directory must exist.
///
/// The resulting snapshot is a complete, independent copy that shares
/// data blocks with the source via BTRFS copy-on-write.
pub fn create_snapshot(source: &Path, dest: &Path) -> Result<()> {
    tracing::debug!(
        source = %source.display(),
        dest = %dest.display(),
        "creating BTRFS snapshot"
    );

    let mut cmd = Command::new("btrfs");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.stdin(Stdio::null());
    // OsStr args: a non-UTF-8 path must not silently collapse to ".".
    let output = cmd
        .arg("subvolume")
        .arg("snapshot")
        .arg(source)
        .arg(dest)
        .output()
        .with_context(|| "failed to execute btrfs subvolume snapshot command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to create BTRFS snapshot from {} to {}: {}",
            source.display(),
            dest.display(),
            stderr.trim()
        );
    }

    tracing::debug!(
        dest = %dest.display(),
        "BTRFS snapshot created successfully"
    );

    Ok(())
}

/// Result of creating a snapshot for a worktree.
#[derive(Debug)]
pub struct SnapshotResult {
    /// The real on-disk btrfs snapshot subvolume. In the symlink case this
    /// lives inside the btrfs mount (e.g., `<btrfs_mount>/worktrees/<name>`);
    /// in the direct case it is `dest` itself.
    pub snapshot_path: PathBuf,
    /// When the snapshot lives inside the btrfs mount, the worktree is exposed
    /// at `dest` via a symlink pointing to `snapshot_path`. This is that symlink
    /// path (equal to `dest`). `None` when the snapshot was created directly at
    /// `dest` (no symlink needed).
    pub symlink_path: Option<PathBuf>,
}

/// Create a BTRFS snapshot, exposing it via a symlink for bind-mounted sources.
///
/// When the source subvolume is accessed via a bind mount (e.g., `/workspace/repo`
/// bind-mounted from `/mnt/btrfs/repo`), the snapshot must be created inside the
/// btrfs mount point (`btrfs subvolume snapshot` requires source and destination
/// on the same btrfs filesystem). This function then exposes it at `dest` via a
/// **symlink** to the on-disk snapshot path.
///
/// A symlink is used instead of a `mount --bind` because a bind mount made in a
/// private mount namespace is invisible to the user's other shells and is torn
/// down when the process restarts. A symlink is an ordinary filesystem object:
/// it crosses mount namespaces and persists across process exits. This mirrors
/// the approach the privileged snapshot delegate uses.
///
/// # Arguments
/// * `btrfs_info` - Information about the source BTRFS subvolume
/// * `dest` - The desired destination path for the worktree
///
/// # Returns
/// A `SnapshotResult` containing the on-disk snapshot path and the optional
/// symlink path created at `dest`.
pub fn create_snapshot_with_symlink(btrfs_info: &BtrfsInfo, dest: &Path) -> Result<SnapshotResult> {
    let snapshot_source = btrfs_info
        .bind_mount_source
        .as_ref()
        .unwrap_or(&btrfs_info.subvolume_root);

    // When the source is directly on btrfs (no bind mount), the snapshot can be
    // created at `dest` itself — it is a real subvolume, not a kernel mount, so
    // it is namespace-independent and persistent. No symlink needed.
    if btrfs_info.bind_mount_source.is_none() {
        create_snapshot(snapshot_source, dest)?;
        return Ok(SnapshotResult {
            snapshot_path: dest.to_path_buf(),
            symlink_path: None,
        });
    }

    // Bind-mount source case: create the snapshot inside the btrfs mount, then
    // symlink `dest` to it.
    let btrfs_mount = btrfs_info.btrfs_mount_point.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "cannot determine btrfs mount point for source {}",
            btrfs_info.subvolume_root.display()
        )
    })?;

    let snapshot_path = snapshot_dest_path(btrfs_mount, &btrfs_info.subvolume_root, dest);
    let worktrees_dir = snapshot_path.parent().unwrap_or(btrfs_mount);
    if !worktrees_dir.exists() {
        std::fs::create_dir_all(worktrees_dir).with_context(|| {
            format!(
                "failed to create worktrees directory at {}",
                worktrees_dir.display()
            )
        })?;
    }

    // A pre-existing entry here would be deleted with btrfs privileges, so guard it:
    // unlink a planted symlink, and only `btrfs delete` a real contained subvolume
    // whose sibling metadata proves it belongs to THIS `dest` (never another session's).
    if snapshot_path.exists() {
        if snapshot_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            std::fs::remove_file(&snapshot_path).with_context(|| {
                format!(
                    "failed to remove planted symlink at {}",
                    snapshot_path.display()
                )
            })?;
        } else if !is_safe_snapshot_delete_target(&snapshot_path) {
            bail!(
                "refusing to delete pre-existing snapshot {}: outside echo-agent-managed \
                 btrfs storage",
                snapshot_path.display()
            );
        } else {
            match snapshot_meta_state(&snapshot_path, dest) {
                // Recreate a stale snapshot for this worktree, or reclaim a
                // crashed-creation orphan whose metadata was never written; the
                // is_safe check above keeps the delete inside managed storage.
                SnapshotMetaState::Matches | SnapshotMetaState::Absent => {
                    tracing::warn!(
                        snapshot_path = %snapshot_path.display(),
                        "snapshot path already exists for this worktree, recreating"
                    );
                    delete_snapshot(&snapshot_path)?;
                }
                // Metadata proves a different session owns it — never delete.
                SnapshotMetaState::Mismatch => bail!(
                    "refusing to delete pre-existing snapshot {}: its metadata targets a \
                     different worktree than {}",
                    snapshot_path.display(),
                    dest.display()
                ),
            }
        }
    }

    tracing::info!(
        source = %snapshot_source.display(),
        snapshot = %snapshot_path.display(),
        dest = %dest.display(),
        "creating BTRFS snapshot with symlink"
    );

    // Create the snapshot inside the btrfs filesystem
    create_snapshot(snapshot_source, &snapshot_path)?;

    // When the snapshot is inside the source (subvol mount case), the snapshot
    // contains an empty .echo-agent-snapshots/ directory (btrfs excludes nested subvolumes
    // from snapshots, leaving only empty directory placeholders). Remove it to
    // keep the worktree clean.
    let stale_snapshots_dir = snapshot_path.join(".echo-agent-snapshots");
    if stale_snapshots_dir.exists()
        && let Err(e) = std::fs::remove_dir(&stale_snapshots_dir)
    {
        tracing::debug!(
            path = %stale_snapshots_dir.display(),
            error = %e,
            "failed to remove stale .echo-agent-snapshots placeholder from snapshot"
        );
    }

    // Expose the snapshot at `dest` via a symlink (namespace-independent). On
    // failure, reclaim the just-created subvolume — at this point it has no
    // symlink and no metadata yet, so neither the removal path nor the orphan
    // scanner could otherwise find it.
    expose_or_reclaim_snapshot(
        dest,
        &snapshot_path,
        create_worktree_symlink,
        delete_snapshot,
    )?;

    tracing::info!(
        snapshot = %snapshot_path.display(),
        dest = %dest.display(),
        "BTRFS snapshot created and symlinked"
    );

    if let Err(e) = write_btrfs_metadata(&snapshot_path, dest) {
        tracing::warn!(error = %e, "failed to write btrfs snapshot metadata");
    }

    Ok(SnapshotResult {
        snapshot_path,
        symlink_path: Some(dest.to_path_buf()),
    })
}

/// Compute the on-disk snapshot subvolume path inside the btrfs mount for a
/// worktree `dest`.
///
/// When the btrfs mount IS the source repo (subvol mount without a separate
/// root mount), snapshots go under `.echo-agent-snapshots/` to stay hidden from git;
/// otherwise they go under `worktrees/`.
///
/// Name is `<basename>-<hash of full dest>`: basename alone collides when two repos
/// share a worktree label on one mount, so one snapshot would clobber the other.
///
/// Shared with the privileged snapshot delegate so both snapshot-creation paths use
/// the identical layout.
pub fn snapshot_dest_path(btrfs_mount: &Path, subvolume_root: &Path, dest: &Path) -> PathBuf {
    let subdir = if btrfs_mount == subvolume_root {
        BTRFS_SNAPSHOT_SUBDIRS[1] // ".echo-agent-snapshots"
    } else {
        BTRFS_SNAPSHOT_SUBDIRS[0] // "worktrees"
    };
    let basename = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot");
    let snapshot_name = format!("{basename}-{}", short_path_hash(dest));
    btrfs_mount.join(subdir).join(snapshot_name)
}

/// Create a symlink at `dest` pointing to `target`, replacing any pre-existing
/// entry (stale symlink or directory) at `dest`.
///
/// Symlinks cross mount namespaces and persist across process exits, so the
/// worktree at `dest` stays visible to the user's other shells and survives a
/// echoagent restart.
///
/// Destructive contract: a pre-existing **stale symlink** at `dest` is unlinked;
/// a pre-existing **directory** is removed only if empty (`remove_dir`). A
/// non-empty directory at `dest` is an error rather than being recursively
/// deleted, so a caller can never silently destroy unrelated data.
///
/// Shared with the privileged snapshot delegate (single implementation of the
/// symlink-clear-then-create logic).
pub fn create_worktree_symlink(dest: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = dest.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory {}", parent.display()))?;
    }

    // Reuse a single `lstat`: a stale symlink is unlinked; a directory is
    // removed only when empty so unrelated data is never recursively destroyed.
    if let Ok(md) = dest.symlink_metadata() {
        if md.file_type().is_symlink() {
            std::fs::remove_file(dest)
                .with_context(|| format!("failed to remove stale symlink at {}", dest.display()))?;
        } else {
            std::fs::remove_dir(dest).with_context(|| {
                format!(
                    "refusing to replace existing non-empty path at {} with a worktree symlink",
                    dest.display()
                )
            })?;
        }
    }

    std::os::unix::fs::symlink(target, dest).with_context(|| {
        format!(
            "failed to symlink {} -> {}",
            dest.display(),
            target.display()
        )
    })
}

/// Expose a freshly created snapshot at `dest`, reclaiming it if exposure fails.
///
/// Runs `symlink(dest, snapshot_path)`; on error, best-effort `reclaim`s the
/// snapshot (it has no symlink and no metadata yet, so nothing else could find
/// it) and propagates the original error. The `symlink`/`reclaim` steps are
/// injected so the failure→cleanup branch is unit-testable without root+btrfs.
fn expose_or_reclaim_snapshot(
    dest: &Path,
    snapshot_path: &Path,
    symlink: impl FnOnce(&Path, &Path) -> Result<()>,
    reclaim: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if let Err(e) = symlink(dest, snapshot_path) {
        let _ = reclaim(snapshot_path);
        return Err(e);
    }
    Ok(())
}

/// Delete a BTRFS subvolume/snapshot.
pub fn delete_snapshot(path: &Path) -> Result<()> {
    let mut cmd = Command::new("btrfs");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.stdin(Stdio::null());
    // Pass the path as OsStr (no lossy `to_str`) so a non-UTF-8 path can never
    // silently collapse to "." and delete the current directory's subvolume.
    let output = cmd
        .arg("subvolume")
        .arg("delete")
        .arg(path)
        .output()
        .with_context(|| "failed to execute btrfs subvolume delete command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to delete BTRFS snapshot at {}: {}",
            path.display(),
            stderr.trim()
        );
    }

    Ok(())
}

/// Validate that `snapshot_path` is a safe target for a privileged
/// `btrfs subvolume delete`.
///
/// Guards against destroying an arbitrary subvolume (e.g. the live source repo,
/// or another session/user's snapshot) via a stale, confused, or planted
/// symlink or `*.btrfs-meta.json` entry. Returns `true` only when the path:
/// - contains no `..` component,
/// - is not itself a symlink (`lstat`, so a planted symlink can't redirect the
///   delete to a subvolume elsewhere),
/// - lives directly inside a snapshot-storage directory — its real,
///   canonicalized parent's final component is one of [`BTRFS_SNAPSHOT_SUBDIRS`]
///   (`worktrees` or `.echo-agent-snapshots`),
/// - and that directory sits **directly under a real btrfs mount point** (from
///   the live mount table), anchoring the delete to echo-agent-managed storage rather
///   than any directory that merely happens to be named `worktrees`.
///
/// Treat all symlink targets and metadata paths as untrusted input and pass
/// them through this check before deleting.
pub fn is_safe_snapshot_delete_target(snapshot_path: &Path) -> bool {
    is_safe_snapshot_delete_target_in(snapshot_path, &btrfs_mount_points())
}

/// Canonicalized-or-raw btrfs mount points from the live mount table.
fn btrfs_mount_points() -> Vec<PathBuf> {
    crate::mount_info::parse_mountinfo()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.fs_type == "btrfs")
        .map(|e| e.mount_point)
        .collect()
}

/// Pure core of [`is_safe_snapshot_delete_target`]; `btrfs_mounts` are the known
/// btrfs mount points (so the containment decision is testable without procfs).
fn is_safe_snapshot_delete_target_in(snapshot_path: &Path, btrfs_mounts: &[PathBuf]) -> bool {
    if snapshot_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    if snapshot_path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return false;
    }
    if snapshot_path.file_name().is_none() {
        return false;
    }
    let Some(parent) = snapshot_path.parent() else {
        return false;
    };
    let Ok(canonical_parent) = dunce::canonicalize(parent) else {
        return false;
    };
    // Parent must be a snapshot-storage dir (`worktrees` / `.echo-agent-snapshots`)...
    let named_ok = canonical_parent
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| BTRFS_SNAPSHOT_SUBDIRS.contains(&name));
    if !named_ok {
        return false;
    }
    // ...sitting directly under a real btrfs mount point.
    let Some(grandparent) = canonical_parent.parent() else {
        return false;
    };
    btrfs_mounts
        .iter()
        .any(|m| m == grandparent || dunce::canonicalize(m).is_ok_and(|c| c == grandparent))
}

/// Metadata persisted alongside a direct btrfs snapshot for crash recovery
/// and orphan scanning.
///
/// Stored as `<snapshot_name>.btrfs-meta.json` next to the snapshot directory
/// (see [`BTRFS_META_SUFFIX`] and [`btrfs_meta_path`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BtrfsSnapshotMetadata {
    #[serde(rename = "type")]
    pub kind: Cow<'static, str>,
    pub snapshot_path: PathBuf,
    pub mount_target: PathBuf,
    pub created_at: String,
}

/// Suffix for btrfs snapshot metadata files (e.g., `wt-abc.btrfs-meta.json`).
pub const BTRFS_META_SUFFIX: &str = ".btrfs-meta.json";

/// Subdirectory names used to store btrfs snapshots inside a btrfs mount point.
///
/// `"worktrees"` is used when a separate btrfs root mount exists (common case).
/// `".echo-agent-snapshots"` is used when the btrfs mount IS the repo subvolume
/// (dot-prefixed to stay hidden from git).
pub const BTRFS_SNAPSHOT_SUBDIRS: &[&str] = &["worktrees", ".echo-agent-snapshots"];

/// Compute the sibling metadata file path for a snapshot directory.
pub fn btrfs_meta_path(snapshot_path: &Path) -> Option<PathBuf> {
    let parent = snapshot_path.parent()?;
    let name = snapshot_path.file_name()?.to_str()?;
    Some(parent.join(format!("{name}{BTRFS_META_SUFFIX}")))
}

/// Write recovery metadata for a btrfs snapshot next to it on disk.
///
/// Public so the privileged snapshot delegate persists the same
/// metadata the in-process path does; without it a snapshot whose `mount_target`
/// symlink is lost is invisible to the orphan scanners and leaks.
pub fn write_btrfs_metadata(snapshot_path: &Path, mount_target: &Path) -> Result<()> {
    let meta_path = btrfs_meta_path(snapshot_path).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot derive metadata path for {}",
            snapshot_path.display()
        )
    })?;

    let meta = BtrfsSnapshotMetadata {
        kind: Cow::Borrowed("btrfs"),
        snapshot_path: snapshot_path.to_path_buf(),
        mount_target: mount_target.to_path_buf(),
        created_at: crate::util::unix_timestamp_string(),
    };

    let content =
        serde_json::to_string_pretty(&meta).context("serialize btrfs snapshot metadata")?;
    std::fs::write(&meta_path, content)
        .with_context(|| format!("write btrfs metadata to {}", meta_path.display()))?;

    tracing::debug!(path = %meta_path.display(), "btrfs snapshot metadata written");
    Ok(())
}

/// Remove metadata for a btrfs snapshot (best-effort).
pub fn remove_btrfs_metadata(snapshot_path: &Path) {
    if let Some(meta_path) = btrfs_meta_path(snapshot_path) {
        let _ = std::fs::remove_file(&meta_path);
    }
}

/// Ownership verdict for a pre-existing snapshot directory, derived from its
/// sibling `*.btrfs-meta.json`. Lets callers tell a reclaimable crashed-creation
/// orphan apart from another session's live snapshot before deleting with btrfs
/// privileges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMetaState {
    /// No sibling metadata file: `create_snapshot` succeeded but
    /// `write_btrfs_metadata` never ran (crash/failure), leaving an orphan at
    /// this dest's hashed path. Reclaimable — still bounded by
    /// [`is_safe_snapshot_delete_target`] so the delete stays in managed storage.
    Absent,
    /// Metadata records `mount_target == dest`: a stale snapshot for this exact
    /// worktree; safe to recreate.
    Matches,
    /// Metadata is present but targets a different dest (or is unreadable /
    /// unparseable / has no derivable path): not provably ours, must not delete.
    Mismatch,
}

/// Classify a pre-existing snapshot directory against `dest` via its sibling
/// metadata. A *missing* meta file is a crashed-creation orphan (reclaimable);
/// a meta file that exists but doesn't prove ownership stays a hard refusal so a
/// retry can't clobber another session's live snapshot.
pub fn snapshot_meta_state(snapshot_path: &Path, dest: &Path) -> SnapshotMetaState {
    let Some(meta_path) = btrfs_meta_path(snapshot_path) else {
        return SnapshotMetaState::Mismatch;
    };
    match std::fs::read_to_string(&meta_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SnapshotMetaState::Absent,
        Err(_) => SnapshotMetaState::Mismatch,
        Ok(content) => match serde_json::from_str::<BtrfsSnapshotMetadata>(&content) {
            Ok(m) if m.mount_target == dest => SnapshotMetaState::Matches,
            _ => SnapshotMetaState::Mismatch,
        },
    }
}

/// Whether the sibling `*.btrfs-meta.json` records `mount_target == dest`.
/// Public so the privileged snapshot delegate gates its delete on the same proof.
pub fn snapshot_meta_targets(snapshot_path: &Path, dest: &Path) -> bool {
    matches!(
        snapshot_meta_state(snapshot_path, dest),
        SnapshotMetaState::Matches
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::detect::{is_btrfs, is_btrfs_subvolume};
    use std::path::PathBuf;

    /// Helper to find a writable BTRFS subvolume for testing.
    /// Returns (subvolume_path, test_dest_path) if available.
    fn get_btrfs_snapshot_test_paths() -> Option<(PathBuf, PathBuf)> {
        // Check environment variable first
        if let Ok(path) = std::env::var("BTRFS_TEST_PATH") {
            let path = PathBuf::from(&path);
            if path.exists()
                && is_btrfs(&path).unwrap_or(false)
                && is_btrfs_subvolume(&path).ok().flatten().is_some()
            {
                let dest = PathBuf::from(format!("{}_snapshot_test", path.display()));
                return Some((path, dest));
            }
        }

        // Check common BTRFS mount points
        for candidate in &["/", "/home"] {
            let path = Path::new(candidate);
            if path.exists()
                && is_btrfs(path).unwrap_or(false)
                && is_btrfs_subvolume(path).ok().flatten().is_some()
            {
                // For system paths, use a temp location
                let dest = PathBuf::from("/tmp/btrfs_snapshot_test");
                return Some((path.to_path_buf(), dest));
            }
        }

        None
    }

    #[test]
    fn test_create_snapshot_nonexistent_source() {
        // Attempting to snapshot a nonexistent path should fail
        let source = Path::new("/nonexistent/source/path");
        let dest = Path::new("/tmp/nonexistent_dest");

        let result = create_snapshot(source, dest);
        assert!(result.is_err());
        // Error could be "failed to create BTRFS snapshot" if btrfs exists,
        // or "failed to execute btrfs" if btrfs command is not available
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("btrfs") || err_msg.contains("BTRFS"),
            "Error should mention btrfs: {}",
            err_msg
        );
    }

    #[test]
    fn test_create_snapshot_non_btrfs_source() {
        // Skip if /tmp happens to be on BTRFS
        if is_btrfs(Path::new("/tmp")).unwrap_or(false) {
            eprintln!("Skipping test: /tmp is on BTRFS");
            return;
        }

        // Attempting to snapshot from a non-BTRFS path should fail
        let source = Path::new("/tmp");
        let dest_path = PathBuf::from("/tmp/test_snapshot_dest_nonbtrfs");

        // Clean up if exists from previous run
        let _ = std::fs::remove_dir_all(&dest_path);

        let result = create_snapshot(source, &dest_path);
        // This should fail because /tmp is not a BTRFS subvolume
        assert!(result.is_err());

        // Clean up
        let _ = std::fs::remove_dir_all(&dest_path);
    }

    #[test]
    fn test_create_snapshot_on_real_btrfs() {
        // This test automatically skips if no BTRFS subvolume is available
        let Some((source_path, dest_path)) = get_btrfs_snapshot_test_paths() else {
            eprintln!("Skipping test: no BTRFS subvolume detected for snapshot test");
            return;
        };

        // Clean up if exists from previous run
        let _ = std::process::Command::new("btrfs")
            .args(["subvolume", "delete", &dest_path.to_string_lossy()])
            .output();
        let _ = std::fs::remove_dir_all(&dest_path);

        // Note: This test may fail if:
        // 1. We don't have permission to create snapshots
        // 2. The dest path is on a different filesystem
        // So we handle both success and expected failures gracefully

        match create_snapshot(&source_path, &dest_path) {
            Ok(()) => {
                eprintln!(
                    "BTRFS snapshot created: {} -> {}",
                    source_path.display(),
                    dest_path.display()
                );
                assert!(dest_path.exists(), "Snapshot should exist");

                // Clean up - use btrfs subvolume delete
                let _ = std::process::Command::new("btrfs")
                    .args(["subvolume", "delete", &dest_path.to_string_lossy()])
                    .output();
            }
            Err(e) => {
                // Expected to fail if we don't have permissions or cross-filesystem
                eprintln!("Snapshot failed (expected if no permissions): {}", e);
            }
        }
    }

    #[test]
    fn test_btrfs_meta_path() {
        let snapshot = Path::new("/mnt/btrfs/worktrees/wt-abc");
        let meta = btrfs_meta_path(snapshot).unwrap();
        assert_eq!(
            meta,
            PathBuf::from("/mnt/btrfs/worktrees/wt-abc.btrfs-meta.json")
        );
    }

    #[test]
    fn test_btrfs_meta_path_root() {
        assert!(btrfs_meta_path(Path::new("/")).is_none());
    }

    #[test]
    fn test_btrfs_snapshot_metadata_round_trip() {
        let meta = BtrfsSnapshotMetadata {
            kind: Cow::Borrowed("btrfs"),
            snapshot_path: PathBuf::from("/mnt/btrfs/worktrees/wt-abc"),
            mount_target: PathBuf::from("/home/user/.echo-agent/worktrees/repo/session/wt-abc"),
            created_at: "1740000000s-since-epoch".to_string(),
        };
        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("\"type\": \"btrfs\""));

        let parsed: BtrfsSnapshotMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, "btrfs");
        assert_eq!(parsed.snapshot_path, meta.snapshot_path);
        assert_eq!(parsed.mount_target, meta.mount_target);
    }

    #[test]
    fn test_write_and_remove_btrfs_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let snapshot_path = tmp.path().join("wt-abc");
        std::fs::create_dir(&snapshot_path).unwrap();
        let mount_target = Path::new("/home/user/.echo-agent/worktrees/wt-abc");

        write_btrfs_metadata(&snapshot_path, mount_target).unwrap();

        let meta_path = btrfs_meta_path(&snapshot_path).unwrap();
        assert!(meta_path.exists());

        let content = std::fs::read_to_string(&meta_path).unwrap();
        let parsed: BtrfsSnapshotMetadata = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.kind, "btrfs");
        assert_eq!(parsed.snapshot_path, snapshot_path);
        assert_eq!(parsed.mount_target, mount_target);

        remove_btrfs_metadata(&snapshot_path);
        assert!(!meta_path.exists());
    }

    #[test]
    fn test_remove_btrfs_metadata_nonexistent() {
        remove_btrfs_metadata(Path::new("/nonexistent/snapshot"));
    }

    #[test]
    fn test_unix_timestamp_string() {
        let ts = crate::util::unix_timestamp_string();
        assert!(ts.ends_with("s-since-epoch"));
        let secs: u64 = ts.strip_suffix("s-since-epoch").unwrap().parse().unwrap();
        assert!(secs > 1_700_000_000);
    }

    /// Assert the snapshot name keeps `<basename>-` and ends in a 16-hex hash.
    fn assert_hashed_name(name: &str, basename: &str) {
        let hash = name
            .strip_prefix(&format!("{basename}-"))
            .unwrap_or_else(|| panic!("name {name:?} must keep the `{basename}-` prefix"));
        assert_eq!(hash.len(), 16, "hash must be 16 hex chars: {name:?}");
        assert!(
            hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "hash must be hex: {name:?}"
        );
    }

    #[test]
    fn test_snapshot_dest_path_separate_root_mount() {
        // btrfs mount differs from subvolume root → snapshots under worktrees/.
        let btrfs_mount = Path::new("/mnt/btrfs");
        let subvolume_root = Path::new("/workspace/repo");
        let dest = Path::new("/home/user/.echo-agent/worktrees/repo/session/wt-abc");
        let got = snapshot_dest_path(btrfs_mount, subvolume_root, dest);
        assert_eq!(got.parent().unwrap(), Path::new("/mnt/btrfs/worktrees"));
        assert_hashed_name(got.file_name().unwrap().to_str().unwrap(), "wt-abc");
        // Deterministic: same dest → same path.
        assert_eq!(snapshot_dest_path(btrfs_mount, subvolume_root, dest), got);
    }

    #[test]
    fn test_snapshot_dest_path_subvol_mount() {
        // btrfs mount IS the subvolume root → snapshots under .echo-agent-snapshots/.
        let mount = Path::new("/workspace/repo");
        let dest = Path::new("/home/user/.echo-agent/worktrees/repo/session/wt-xyz");
        let got = snapshot_dest_path(mount, mount, dest);
        assert_eq!(
            got.parent().unwrap(),
            Path::new("/workspace/repo/.echo-agent-snapshots")
        );
        assert_hashed_name(got.file_name().unwrap().to_str().unwrap(), "wt-xyz");
    }

    #[test]
    fn test_snapshot_dest_path_dest_without_filename() {
        let btrfs_mount = Path::new("/mnt/btrfs");
        let subvolume_root = Path::new("/workspace/repo");
        let got = snapshot_dest_path(btrfs_mount, subvolume_root, Path::new("/"));
        assert_eq!(got.parent().unwrap(), Path::new("/mnt/btrfs/worktrees"));
        assert_hashed_name(got.file_name().unwrap().to_str().unwrap(), "snapshot");
    }

    #[test]
    fn test_snapshot_dest_path_disambiguates_same_basename_across_repos() {
        // Two repos' same-label worktrees on one btrfs mount must NOT map to the
        // same on-disk snapshot (the cross-repo data-loss collision).
        let btrfs_mount = Path::new("/mnt/btrfs");
        let subvolume_root = Path::new("/workspace/repo");
        let dest_a = Path::new("/home/user/.echo-agent/worktrees/repo-a/session/wt-abc");
        let dest_b = Path::new("/home/user/.echo-agent/worktrees/repo-b/session/wt-abc");
        let a = snapshot_dest_path(btrfs_mount, subvolume_root, dest_a);
        let b = snapshot_dest_path(btrfs_mount, subvolume_root, dest_b);
        assert_ne!(
            a, b,
            "same-basename worktrees in different repos must get distinct snapshot paths"
        );
        assert_hashed_name(a.file_name().unwrap().to_str().unwrap(), "wt-abc");
        assert_hashed_name(b.file_name().unwrap().to_str().unwrap(), "wt-abc");
    }

    /// Unit-tests only the metadata-matching helper; the delete integration that
    /// consumes it needs a privileged+btrfs host, so it is not covered here.
    #[test]
    fn test_snapshot_meta_targets_matches_only_same_dest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let snapshot_path = tmp.path().join("worktrees").join("wt-abc-deadbeef");
        std::fs::create_dir_all(&snapshot_path).unwrap();
        let dest = Path::new("/home/user/.echo-agent/worktrees/repo-a/session/wt-abc");

        // No metadata yet → cannot prove ownership → refuse.
        assert!(!snapshot_meta_targets(&snapshot_path, dest));

        // Metadata for the SAME dest → may recreate.
        write_btrfs_metadata(&snapshot_path, dest).unwrap();
        assert!(snapshot_meta_targets(&snapshot_path, dest));

        // Metadata for a DIFFERENT dest → must refuse (would clobber other session).
        let other = Path::new("/home/user/.echo-agent/worktrees/repo-b/session/wt-abc");
        assert!(!snapshot_meta_targets(&snapshot_path, other));
    }

    #[test]
    fn snapshot_meta_state_distinguishes_orphan_match_and_foreign() {
        let tmp = tempfile::TempDir::new().unwrap();
        let snapshot_path = tmp.path().join("worktrees").join("wt-abc-deadbeef");
        std::fs::create_dir_all(&snapshot_path).unwrap();
        let dest = Path::new("/home/user/.echo-agent/worktrees/repo-a/session/wt-abc");

        // No sibling meta → crashed-creation orphan → reclaimable.
        assert_eq!(
            snapshot_meta_state(&snapshot_path, dest),
            SnapshotMetaState::Absent
        );

        // Meta records this dest → stale snapshot for this worktree → recreate.
        write_btrfs_metadata(&snapshot_path, dest).unwrap();
        assert_eq!(
            snapshot_meta_state(&snapshot_path, dest),
            SnapshotMetaState::Matches
        );

        // Meta records a different dest → another session → must refuse.
        let other = Path::new("/home/user/.echo-agent/worktrees/repo-b/session/wt-abc");
        assert_eq!(
            snapshot_meta_state(&snapshot_path, other),
            SnapshotMetaState::Mismatch
        );

        // Present but unparseable meta → not provably ours → refuse (conservative).
        std::fs::write(btrfs_meta_path(&snapshot_path).unwrap(), b"{ not json").unwrap();
        assert_eq!(
            snapshot_meta_state(&snapshot_path, dest),
            SnapshotMetaState::Mismatch
        );
    }

    #[test]
    fn test_create_worktree_symlink_resolves_to_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("snapshot");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("file.txt"), b"hello").unwrap();

        let dest = tmp.path().join("nested").join("worktree");
        create_worktree_symlink(&dest, &target).unwrap();

        // dest is a symlink that resolves to the snapshot directory.
        assert!(dest.is_symlink());
        assert_eq!(std::fs::read_link(&dest).unwrap(), target);
        // The file in the snapshot is readable through the symlink.
        assert_eq!(
            std::fs::read_to_string(dest.join("file.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_create_worktree_symlink_replaces_existing_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let old_target = tmp.path().join("old");
        let new_target = tmp.path().join("new");
        std::fs::create_dir(&old_target).unwrap();
        std::fs::create_dir(&new_target).unwrap();

        let dest = tmp.path().join("worktree");
        create_worktree_symlink(&dest, &old_target).unwrap();
        assert_eq!(std::fs::read_link(&dest).unwrap(), old_target);

        // Re-creating replaces the stale symlink.
        create_worktree_symlink(&dest, &new_target).unwrap();
        assert!(dest.is_symlink());
        assert_eq!(std::fs::read_link(&dest).unwrap(), new_target);
    }

    #[test]
    fn test_create_worktree_symlink_replaces_empty_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("snapshot");
        std::fs::create_dir(&target).unwrap();

        // An empty real directory (e.g. left by a previous bind-mount layout).
        let dest = tmp.path().join("worktree");
        std::fs::create_dir(&dest).unwrap();

        create_worktree_symlink(&dest, &target).unwrap();
        assert!(dest.is_symlink());
        assert_eq!(std::fs::read_link(&dest).unwrap(), target);
    }

    #[test]
    fn test_create_worktree_symlink_refuses_non_empty_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("snapshot");
        std::fs::create_dir(&target).unwrap();

        // A populated real directory at dest must NOT be recursively destroyed.
        let dest = tmp.path().join("worktree");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("leftover.txt"), b"precious").unwrap();

        let err = create_worktree_symlink(&dest, &target).unwrap_err();
        assert!(
            err.to_string().contains("refusing to replace"),
            "unexpected error: {err}"
        );
        // The directory and its contents are preserved; no symlink was created.
        assert!(!dest.is_symlink());
        assert_eq!(
            std::fs::read_to_string(dest.join("leftover.txt")).unwrap(),
            "precious"
        );
    }

    #[test]
    fn test_create_worktree_symlink_parent_is_file_fails_cleanly() {
        // When dest's parent is a regular file, `create_dir_all` fails: assert
        // an error and that no partial symlink is left behind.
        let tmp = tempfile::TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"i am a file").unwrap();

        let dest = blocker.join("subdir").join("worktree");
        let target = tmp.path().join("snapshot");
        std::fs::create_dir(&target).unwrap();

        assert!(create_worktree_symlink(&dest, &target).is_err());
        assert!(dest.symlink_metadata().is_err(), "no partial state");
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_accepts_contained() {
        // The tmp dir stands in for a real btrfs mount point.
        let tmp = tempfile::TempDir::new().unwrap();
        let mount = dunce::canonicalize(tmp.path()).unwrap();
        for subdir in BTRFS_SNAPSHOT_SUBDIRS {
            let dir = tmp.path().join(subdir);
            std::fs::create_dir(&dir).unwrap();
            // The candidate need not exist; only its parent is canonicalized.
            assert!(
                is_safe_snapshot_delete_target_in(
                    &dir.join("snap-1"),
                    std::slice::from_ref(&mount)
                ),
                "{subdir}/snap-1 should be accepted"
            );
        }
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_rejects_unanchored_worktrees_dir() {
        // A subvolume under a dir merely *named* `worktrees` that is NOT directly
        // under a btrfs mount point must be refused (anchoring).
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("worktrees");
        std::fs::create_dir(&dir).unwrap();
        let candidate = dir.join("victim");
        // No btrfs mounts known → not anchored → reject.
        assert!(!is_safe_snapshot_delete_target_in(&candidate, &[]));
        // A btrfs mount that is NOT the grandparent → still reject.
        assert!(!is_safe_snapshot_delete_target_in(
            &candidate,
            &[PathBuf::from("/some/other/btrfs/mount")]
        ));
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_rejects_repo_root() {
        // A symlink confused to point at the live source repo: its parent is not
        // a snapshot-storage dir, so deletion must be refused.
        let tmp = tempfile::TempDir::new().unwrap();
        let mount = dunce::canonicalize(tmp.path()).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        assert!(!is_safe_snapshot_delete_target_in(&repo, &[mount]));
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_rejects_parent_dir_component() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mount = dunce::canonicalize(tmp.path()).unwrap();
        let dir = tmp.path().join("worktrees");
        std::fs::create_dir(&dir).unwrap();
        // `worktrees/../escape` escapes the snapshot storage despite the name.
        assert!(!is_safe_snapshot_delete_target_in(
            &dir.join("..").join("escape"),
            &[mount]
        ));
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_rejects_symlink_itself() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mount = dunce::canonicalize(tmp.path()).unwrap();
        let dir = tmp.path().join("worktrees");
        std::fs::create_dir(&dir).unwrap();
        let real = tmp.path().join("elsewhere");
        std::fs::create_dir(&real).unwrap();
        // A planted symlink at worktrees/<name> -> elsewhere must be rejected.
        let link = dir.join("snap-evil");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(!is_safe_snapshot_delete_target_in(&link, &[mount]));
    }

    #[test]
    fn test_is_safe_snapshot_delete_target_rejects_nonexistent_parent() {
        // No canonicalizable parent → cannot prove containment → reject.
        assert!(!is_safe_snapshot_delete_target_in(
            Path::new("/nonexistent/worktrees/snap"),
            &[PathBuf::from("/nonexistent")]
        ));
    }

    #[test]
    fn test_expose_or_reclaim_snapshot_success_does_not_reclaim() {
        let reclaimed = std::cell::Cell::new(false);
        let res = expose_or_reclaim_snapshot(
            Path::new("/dest"),
            Path::new("/snap"),
            |_, _| Ok(()),
            |_| {
                reclaimed.set(true);
                Ok(())
            },
        );
        assert!(res.is_ok());
        assert!(
            !reclaimed.get(),
            "reclaim must not run when symlink succeeds"
        );
    }

    #[test]
    fn test_expose_or_reclaim_snapshot_failure_reclaims_orphan() {
        let reclaimed = std::cell::Cell::new(None);
        let snap = Path::new("/snap");
        let res = expose_or_reclaim_snapshot(
            Path::new("/dest"),
            snap,
            |_, _| anyhow::bail!("symlink failed"),
            |p| {
                reclaimed.set(Some(p.to_path_buf()));
                Ok(())
            },
        );
        assert!(res.is_err(), "original error must propagate");
        assert_eq!(
            reclaimed.into_inner().as_deref(),
            Some(snap),
            "reclaim must run on the orphaned snapshot when symlink fails"
        );
    }
}
