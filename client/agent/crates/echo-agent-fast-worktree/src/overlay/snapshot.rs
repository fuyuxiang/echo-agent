//! Overlay worktree creation and removal.
//!
//! Orchestrates btrfs snapshot of the overlay upper dir, metadata persistence,
//! overlayfs mount, and cleanup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::detect::OverlayInfo;
use crate::api::RemoveReport;
use crate::btrfs::snapshot::create_snapshot;
use crate::util::unix_timestamp_string;

/// Result of creating an overlay worktree.
#[derive(Debug)]
#[allow(dead_code)]
pub struct OverlayWorktreeResult {
    /// The final worktree path (the overlay mount target).
    pub worktree_path: PathBuf,
    /// The btrfs snapshot root (the subvolume, for cleanup).
    pub snapshot_root: PathBuf,
    /// The work dir path (for cleanup).
    pub work_dir: PathBuf,
}

/// Metadata persisted alongside the snapshot for crash recovery.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OverlayMetadata {
    /// Always "overlay".
    #[serde(rename = "type")]
    kind: String,
    /// Path to the btrfs snapshot root (the subvolume to pass to `btrfs subvolume delete`).
    ///
    /// - **New layout:** `<wt_base>/root` — the entire overlay_root is snapshotted,
    ///   and `root/upper/` is the overlayfs upper dir.
    /// - **Old layout (via alias):** `<wt_base>/upper` — the upper dir was itself the
    ///   btrfs subvolume and also the overlayfs upper dir.
    ///
    /// In both cases, this field is the correct path for `btrfs subvolume delete`.
    #[serde(alias = "snapshot_upper")]
    snapshot_root: PathBuf,
    /// Path to the overlay work dir.
    work_dir: PathBuf,
    /// Path to the FUSE lower dir.
    lower_dir: PathBuf,
    /// Path where the overlay was mounted.
    mount_target: PathBuf,
    /// ISO 8601 timestamp.
    created_at: String,
}

const METADATA_FILENAME: &str = ".fast-worktree-meta.json";

/// Create an overlay worktree: snapshot upper → write metadata → mount overlay.
///
/// # Arguments
/// * `info` - The detected FUSE+overlay info for the source repo.
/// * `dest` - Where the worktree should appear (the overlay mount target).
pub fn create_overlay_worktree(
    info: &OverlayInfo,
    dest: &Path,
    delegate: Option<&std::sync::Arc<dyn crate::BtrfsDelegate>>,
) -> Result<OverlayWorktreeResult> {
    // Derive worktree name from dest path.
    let wt_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("overlay-wt");

    // Layout: <overlay_root>/worktrees/<wt_name>/root (btrfs snapshot)
    //   root/upper        — the snapshot's upper dir (used as overlayfs upperdir)
    //   root/overlay-work — the overlay work dir (created fresh; see below)
    //   root/work         — inert copy of the source's work dir, reclaimed with
    //                       the snapshot subvolume (not used by this worktree)
    //
    // IMPORTANT: work dir MUST be inside the snapshot (same btrfs subvolume as
    // upper). Btrfs snapshots get their own subvolume with a distinct device ID.
    // If work is outside the snapshot (different device), overlayfs returns
    // EXDEV ("Invalid cross-device link") on unlink() — breaking git and most
    // file-replacing workflows. This was the case with the old layout where
    // work lived at <wt_base>/work (parent subvolume, different device ID).
    let wt_base = info.overlay_root.join("worktrees").join(wt_name);
    let snapshot_root = wt_base.join("root");
    // Dedicated work dir, not the source's `work/`: the snapshot copies that
    // `work/` whose root-owned, mode-000 internals a rootless creator can't
    // delete, so a fresh name avoids that cleanup. (Still inside the snapshot
    // per the same-subvolume requirement above; the stale copy is reclaimed
    // with the snapshot subvolume.)
    let work_dir = snapshot_root.join("overlay-work");

    // Clean up if a previous attempt left debris.
    if snapshot_root.exists() {
        tracing::warn!(
            snapshot = %snapshot_root.display(),
            "stale snapshot exists, deleting"
        );
        let _ = delete_btrfs_snapshot(&snapshot_root);
    }
    // Clean up old-layout work dir (was at wt_base/work, now inside snapshot).
    let old_work_dir = wt_base.join("work");
    if old_work_dir.exists() {
        let _ = std::fs::remove_dir_all(&old_work_dir);
    }

    // Ensure parent directories exist.
    std::fs::create_dir_all(&wt_base)
        .with_context(|| format!("create worktree base dir {}", wt_base.display()))?;

    // Step 1: Snapshot the overlay root (the btrfs subvolume).
    //
    // The overlay_root is the btrfs subvolume; upper_dir is a regular directory
    // inside it — not a subvolume itself. `btrfs subvolume snapshot` requires the
    // source to be a subvolume, so we snapshot overlay_root and then use the
    // `upper/` subdirectory from within the snapshot.
    tracing::debug!(
        source = %info.overlay_root.display(),
        dest = %snapshot_root.display(),
        "snapshotting overlay root (btrfs subvolume)"
    );
    create_snapshot(&info.overlay_root, &snapshot_root)
        .context("failed to snapshot overlay root")?;

    // The snapshot's upper dir is at the same relative position inside the snapshot.
    let snapshot_upper = snapshot_root.join("upper");

    // Step 2: Create the worktree's overlay work dir (fresh + empty). The
    // snapshot was just (re)created from overlay_root, which has no
    // `overlay-work` entry, so this name never pre-exists.
    std::fs::create_dir(&work_dir)
        .with_context(|| format!("create overlay work dir {}", work_dir.display()))?;

    // Step 3: Write metadata for crash recovery.
    // Written to wt_base (not inside the snapshot) so it survives overlay unmount.
    write_metadata(&wt_base, &snapshot_root, &work_dir, &info.lower_dir, dest)?;

    // Step 4: Mount overlay at dest.
    std::fs::create_dir_all(dest)
        .with_context(|| format!("create overlay mount target {}", dest.display()))?;

    // Rootless callers delegate the mount to a privileged helper (it mounts in
    // our namespace); callers with full caps mount in-process.
    let mount_result = match delegate {
        Some(d) => d.mount_overlay(&info.lower_dir, &snapshot_upper, &work_dir, dest),
        None => mount_overlay(&info.lower_dir, &snapshot_upper, &work_dir, dest),
    };
    if let Err(e) = mount_result {
        // Clean up the snapshot if mount fails.
        tracing::warn!(error = %e, "overlay mount failed, cleaning up snapshot");
        let _ = delete_btrfs_snapshot(&snapshot_root);
        let _ = std::fs::remove_dir_all(&work_dir);
        let _ = std::fs::remove_dir_all(&wt_base);
        return Err(e.context("mount overlay"));
    }

    tracing::info!(
        dest = %dest.display(),
        overlay_upper = %snapshot_upper.display(),
        snapshot_root = %snapshot_root.display(),
        "overlay worktree mounted"
    );

    Ok(OverlayWorktreeResult {
        worktree_path: dest.to_path_buf(),
        snapshot_root,
        work_dir,
    })
}

/// Remove an overlay worktree: unmount → delete snapshot → cleanup.
///
/// `delegate` is `Some` for rootless callers that lack `CAP_SYS_ADMIN`; the
/// overlay unmount is then delegated to a privileged helper (mirroring the
/// create path). Privileged callers (e.g. an orphan-cleanup job) pass `None`
/// and unmount in-process.
pub fn remove_overlay_worktree(
    target: &Path,
    snapshot_root: &Path,
    work_dir: &Path,
    delegate: Option<&std::sync::Arc<dyn crate::BtrfsDelegate>>,
) -> Result<RemoveReport> {
    // Unmount the overlay (delegated when rootless; see fn docs).
    let unmount_result = match delegate {
        Some(d) => d.unmount_overlay(target),
        None => unmount_overlay(target),
    };
    if let Err(e) = unmount_result {
        tracing::warn!(
            target = %target.display(),
            error = %e,
            "overlay unmount failed (may already be unmounted)"
        );
    }

    // Cross-namespace safety gate: an in-process (`delegate: None`) unmount can't
    // detach an overlay living in another process's namespace, so before deleting
    // the backing snapshot confirm it's unmounted in ANY namespace — else we'd
    // reclaim the lower/upper under a live worktree. upperdir is
    // `<snapshot_root>/upper` (new layout) or `<snapshot_root>` (old).
    let overlay_upper = if snapshot_root.file_name().is_some_and(|n| n == "root") {
        snapshot_root.join("upper")
    } else {
        snapshot_root.to_path_buf()
    };
    if crate::mount_info::overlay_upperdirs_all_namespaces().contains(&overlay_upper) {
        anyhow::bail!(
            "refusing to remove overlay worktree {}: still mounted (upper {}) — \
             likely in another mount namespace this caller can't detach",
            target.display(),
            overlay_upper.display()
        );
    }

    // Remove the (now empty) mount point directory.
    let _ = std::fs::remove_dir(target);

    // Delete the btrfs snapshot (subvolume). Best-effort so we don't skip
    // cleaning up work dirs, metadata, and parent dirs on failure — orphan
    // cleanup reclaims leftover snapshots on a later run.
    let mut snapshot_delete_err = None;
    if snapshot_root.exists()
        && let Err(e) = delete_btrfs_snapshot(snapshot_root)
    {
        tracing::warn!(
            path = %snapshot_root.display(),
            error = %e,
            "failed to delete overlay btrfs snapshot, continuing cleanup"
        );
        snapshot_delete_err = Some(e);
    }

    // Remove work dir (only relevant for old layout where work dir is outside
    // the snapshot; for new layout it's inside and already gone with the snapshot).
    let _ = std::fs::remove_dir_all(work_dir);

    // Clean up the metadata file — it lives outside the snapshot (at wt_base
    // level), so snapshot deletion above doesn't remove it.
    if let Some(wt_base) = snapshot_root.parent() {
        let meta_path = wt_base.join(METADATA_FILENAME);
        let _ = std::fs::remove_file(&meta_path);

        // Remove parent (worktrees/<name>/) if now empty.
        let _ = std::fs::remove_dir(wt_base);
    }

    tracing::info!(
        target = %target.display(),
        snapshot = %snapshot_root.display(),
        "overlay worktree removed"
    );

    // Report snapshot deletion failure after completing all other cleanup.
    if let Some(err) = snapshot_delete_err {
        return Err(err.context(format!(
            "overlay worktree at {} partially removed: btrfs snapshot {} could not be deleted",
            target.display(),
            snapshot_root.display()
        )));
    }

    Ok(RemoveReport {
        used_btrfs_delete: true,
        unmounted_bind: false,
        unmounted_overlay: true,
    })
}

/// Try to remove via live mountinfo (Method 1).
pub fn try_remove_from_mountinfo(
    target: &Path,
    delegate: Option<&std::sync::Arc<dyn crate::BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    let entries = match crate::mount_info::parse_mountinfo() {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    let target_str = target.to_string_lossy();
    let overlay_entry = entries
        .iter()
        .find(|e| e.fs_type == "overlay" && e.mount_point.to_string_lossy() == target_str);

    let entry = match overlay_entry {
        Some(e) => e,
        None => return Ok(None),
    };

    // Extract upperdir and workdir from mount options.
    let upper_dir = match crate::mount_info::extract_option(&entry.super_options, "upperdir") {
        Some(v) => PathBuf::from(v),
        None => return Ok(None),
    };
    let work_dir = match crate::mount_info::extract_option(&entry.super_options, "workdir") {
        Some(v) => PathBuf::from(v),
        None => return Ok(None),
    };

    // The overlayfs upperdir path always ends with `/upper` in both layouts:
    // - New: `.../worktrees/<name>/root/upper` → snapshot subvol = parent (`.../root`)
    // - Old: `.../worktrees/<name>/upper` → snapshot subvol = upper_dir itself
    let snapshot_root = if let Some(parent) = upper_dir.parent() {
        if parent.ends_with("root") {
            parent.to_path_buf()
        } else {
            upper_dir.clone()
        }
    } else {
        upper_dir.clone()
    };

    tracing::info!(
        target = %target.display(),
        upper = %upper_dir.display(),
        snapshot_root = %snapshot_root.display(),
        "detected overlay mount via mountinfo, removing"
    );

    remove_overlay_worktree(target, &snapshot_root, &work_dir, delegate).map(Some)
}

/// Try to remove via persisted metadata (Method 2 — crash recovery).
///
/// Scans known overlay roots under `/local/repo-fuse-*/worktrees/*/` for
/// `.fast-worktree-meta.json` files whose `mount_target` matches `target`.
/// This works even after the overlay has been unmounted — the metadata lives
/// on the btrfs filesystem next to the snapshot upper dir.
pub fn try_remove_from_metadata(
    target: &Path,
    delegate: Option<&std::sync::Arc<dyn crate::BtrfsDelegate>>,
) -> Result<Option<RemoveReport>> {
    // Scan common overlay roots for metadata files.
    let local = Path::new("/local");
    if !local.exists() {
        return Ok(None);
    }

    let target_str = target.to_string_lossy();

    let Ok(entries) = std::fs::read_dir(local) else {
        return Ok(None);
    };

    for dir_entry in entries.flatten() {
        let name = dir_entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("repo-fuse-") {
            continue;
        }

        let worktrees_dir = dir_entry.path().join("worktrees");
        let Ok(wt_entries) = std::fs::read_dir(&worktrees_dir) else {
            continue;
        };

        for wt_entry in wt_entries.flatten() {
            let meta_path = wt_entry.path().join(METADATA_FILENAME);
            if !meta_path.exists() {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<OverlayMetadata>(&content) else {
                continue;
            };

            if meta.mount_target.to_string_lossy() == target_str {
                tracing::info!(
                    target = %target.display(),
                    snapshot = %meta.snapshot_root.display(),
                    meta_path = %meta_path.display(),
                    "found overlay metadata via filesystem scan"
                );
                return remove_overlay_worktree(
                    target,
                    &meta.snapshot_root,
                    &meta.work_dir,
                    delegate,
                )
                .map(Some);
            }
        }
    }

    Ok(None)
}

/// Scan known overlay roots under `/local/repo-fuse-*/worktrees/` for orphaned
/// overlay snapshots.
///
/// An overlay snapshot is orphaned if:
/// - Its metadata file exists but the `mount_target` doesn't exist or isn't mounted
/// - Or the worktrees/ dir contains snapshot dirs without metadata (crashed mid-create)
///
/// For each orphan: delete the btrfs snapshot, remove the work dir,
/// remove the metadata file, clean up the parent dir.
///
/// Intended for host startup / periodic cleanup of leftovers left behind when
/// a previous session exited uncleanly.
pub fn cleanup_orphaned_overlay_snapshots() -> crate::api::CleanupReport {
    let mut report = crate::api::CleanupReport::default();

    let local = Path::new("/local");
    if !local.exists() {
        return report;
    }

    let Ok(entries) = std::fs::read_dir(local) else {
        return report;
    };

    // Active overlay upperdirs across ALL namespaces (overlays may live in
    // another process's namespace) — never delete a snapshot still backing a
    // mounted overlay.
    let active_uppers = crate::mount_info::overlay_upperdirs_all_namespaces();

    for dir_entry in entries.flatten() {
        let name = dir_entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("repo-fuse-") {
            continue;
        }

        let worktrees_dir = dir_entry.path().join("worktrees");
        let Ok(wt_entries) = std::fs::read_dir(&worktrees_dir) else {
            continue;
        };

        for wt_entry in wt_entries.flatten() {
            let wt_path = wt_entry.path();
            let meta_path = wt_path.join(METADATA_FILENAME);
            let work_dir = wt_path.join("work");

            // Detect layout: new layout has `root/` (snapshot), old has `upper/` (snapshot).
            // The overlayfs upperdir is `root/upper` (new) or `upper/` (old).
            let (snapshot_path, overlay_upper) = {
                let new_root = wt_path.join("root");
                if new_root.exists() {
                    let upper = new_root.join("upper");
                    (new_root, upper)
                } else {
                    let old_upper = wt_path.join("upper");
                    (old_upper.clone(), old_upper)
                }
            };

            // Still mounted in any namespace? (upperdir match, both layouts)
            let is_active = active_uppers.contains(&overlay_upper);

            if is_active {
                tracing::debug!(
                    snapshot = %snapshot_path.display(),
                    "skipping active overlay snapshot"
                );
                continue;
            }

            // Try to read metadata for additional cleanup (unmount stale target).
            if let Ok(content) = std::fs::read_to_string(&meta_path)
                && let Ok(meta) = serde_json::from_str::<OverlayMetadata>(&content)
            {
                tracing::info!(
                    target = %meta.mount_target.display(),
                    snapshot = %meta.snapshot_root.display(),
                    "cleaning up orphaned overlay snapshot"
                );

                // Unmount target if it's somehow still a mountpoint (stale).
                let _ = unmount_overlay(&meta.mount_target);
                let _ = std::fs::remove_dir(&meta.mount_target);
            } else {
                tracing::info!(
                    snapshot = %snapshot_path.display(),
                    "cleaning up orphaned snapshot (no or corrupt metadata)"
                );
            }

            // Clean up btrfs subvolume + work dir + metadata.
            if snapshot_path.exists() {
                if let Err(e) = delete_btrfs_snapshot(&snapshot_path) {
                    tracing::warn!(
                        path = %snapshot_path.display(),
                        error = %e,
                        "failed to delete orphaned btrfs snapshot"
                    );
                    report.errors += 1;
                    // Still clean up metadata + work dir below
                } else {
                    report.btrfs_deleted += 1;
                }
            }

            let _ = std::fs::remove_dir_all(&work_dir);
            let _ = std::fs::remove_file(&meta_path);
            let _ = std::fs::remove_dir(&wt_path);
            report.removed += 1;
        }

        // Remove the worktrees/ dir if now empty.
        let _ = std::fs::remove_dir(&worktrees_dir);
    }

    if report.removed > 0 || report.errors > 0 {
        tracing::info!(
            removed = report.removed,
            btrfs = report.btrfs_deleted,
            errors = report.errors,
            "orphaned overlay snapshot cleanup complete"
        );
    }

    report
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Mount overlayfs using `libc::mount()` syscall.
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, target: &Path) -> Result<()> {
    use std::ffi::CString;

    // index=on enables correct rename() and hardlink semantics.
    let mount_data = format!(
        "lowerdir={},upperdir={},workdir={},index=on",
        lower.display(),
        upper.display(),
        work.display(),
    );

    tracing::debug!(
        lower = %lower.display(),
        upper = %upper.display(),
        work = %work.display(),
        target = %target.display(),
        "mounting overlayfs"
    );

    let c_source = CString::new("overlay").unwrap();
    let c_target =
        CString::new(target.as_os_str().as_encoded_bytes()).context("target path not C-safe")?;
    let c_fstype = CString::new("overlay").unwrap();
    let c_data = CString::new(mount_data.as_str()).context("mount data not C-safe")?;

    // SAFETY: all pointers are valid CStrings that outlive the call.
    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            0,
            c_data.as_ptr().cast(),
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        bail!("mount overlay at {}: {err}", target.display());
    }

    Ok(())
}

/// Unmount a filesystem (lazy/detach to avoid EBUSY).
fn unmount_overlay(target: &Path) -> Result<()> {
    use std::ffi::CString;

    let c_target =
        CString::new(target.as_os_str().as_encoded_bytes()).context("target path not C-safe")?;

    // SAFETY: c_target is a valid CString.
    let rc = unsafe { libc::umount2(c_target.as_ptr(), libc::MNT_DETACH) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        bail!("unmount {}: {err}", target.display());
    }

    Ok(())
}

/// Delete a btrfs subvolume/snapshot (delegates to shared btrfs module).
fn delete_btrfs_snapshot(path: &Path) -> Result<()> {
    crate::btrfs::snapshot::delete_snapshot(path)
}

/// Write metadata JSON to the worktree base dir for crash recovery.
///
/// Written to `<wt_base>/.fast-worktree-meta.json` (next to `upper/` and
/// `work/` dirs), NOT inside the overlay. This ensures the metadata is
/// always readable from the btrfs filesystem regardless of overlay mount state.
fn write_metadata(
    wt_base: &Path,
    snapshot_root: &Path,
    work_dir: &Path,
    lower_dir: &Path,
    mount_target: &Path,
) -> Result<()> {
    let meta = OverlayMetadata {
        kind: "overlay".to_string(),
        snapshot_root: snapshot_root.to_path_buf(),
        work_dir: work_dir.to_path_buf(),
        lower_dir: lower_dir.to_path_buf(),
        mount_target: mount_target.to_path_buf(),
        created_at: unix_timestamp_string(),
    };

    let meta_path = wt_base.join(METADATA_FILENAME);
    let content = serde_json::to_string_pretty(&meta).context("serialize overlay metadata")?;
    std::fs::write(&meta_path, &content)
        .with_context(|| format!("write overlay metadata to {}", meta_path.display()))?;

    tracing::debug!(path = %meta_path.display(), "overlay metadata written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_serialization() {
        let meta = OverlayMetadata {
            kind: "overlay".to_string(),
            snapshot_root: PathBuf::from("/var/lib/repo-fuse/instance/worktrees/abc/root"),
            work_dir: PathBuf::from("/var/lib/repo-fuse/instance/worktrees/abc/work"),
            lower_dir: PathBuf::from("/var/lib/repo-fuse/instance/fuse-lower"),
            mount_target: PathBuf::from("/home/user/.echo-agent/worktrees/abc"),
            created_at: "2026-02-19T22:38:00Z".to_string(),
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        assert!(json.contains("\"type\": \"overlay\""));
        assert!(json.contains("snapshot_root"));
        assert!(json.contains("work_dir"));
        assert!(json.contains("lower_dir"));
        assert!(json.contains("mount_target"));

        // Round-trip.
        let parsed: OverlayMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, "overlay");
        assert_eq!(parsed.snapshot_root, meta.snapshot_root);
        assert_eq!(parsed.work_dir, meta.work_dir);
        assert_eq!(parsed.lower_dir, meta.lower_dir);
        assert_eq!(parsed.mount_target, meta.mount_target);
    }

    #[test]
    fn test_metadata_deserialization_from_fixture() {
        let json = r#"{
            "type": "overlay",
            "snapshot_upper": "/var/lib/repo-fuse/instance/worktrees/abc123/upper",
            "work_dir": "/var/lib/repo-fuse/instance/worktrees/abc123/work",
            "lower_dir": "/var/lib/repo-fuse/instance/fuse-lower",
            "mount_target": "/home/user/.echo-agent/worktrees/abc123",
            "created_at": "2026-02-19T22:38:00Z"
        }"#;

        let meta: OverlayMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.kind, "overlay");
        // "snapshot_upper" in JSON is deserialized into snapshot_root via serde alias
        assert_eq!(
            meta.snapshot_root,
            PathBuf::from("/var/lib/repo-fuse/instance/worktrees/abc123/upper")
        );
    }

    #[test]
    fn test_unix_timestamp_string() {
        let ts = unix_timestamp_string();
        assert!(ts.contains("s-since-epoch"));
    }

    #[test]
    fn test_overlay_worktree_result_debug() {
        let result = OverlayWorktreeResult {
            worktree_path: PathBuf::from("/dest"),
            snapshot_root: PathBuf::from("/snap/root"),
            work_dir: PathBuf::from("/snap/work"),
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("OverlayWorktreeResult"));
        assert!(debug.contains("/dest"));
    }

    #[test]
    fn test_try_remove_from_mountinfo_no_overlay() {
        // When the target isn't an overlay mount, should return None.
        let result = try_remove_from_mountinfo(Path::new("/tmp/nonexistent"), None);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_try_remove_from_metadata_no_local() {
        // When /local doesn't exist or has no repo-fuse dirs, should return None.
        let result = try_remove_from_metadata(Path::new("/tmp/nonexistent-worktree"), None);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_metadata_written_to_wt_base() {
        // Verify that write_metadata creates the file in wt_base, not inside the snapshot.
        let tmp = tempfile::TempDir::new().unwrap();
        let wt_base = tmp.path().join("wt_base");
        let snapshot_root = wt_base.join("root");
        let work_dir = wt_base.join("work");
        std::fs::create_dir_all(&snapshot_root).unwrap();
        std::fs::create_dir_all(&work_dir).unwrap();

        write_metadata(
            &wt_base,
            &snapshot_root,
            &work_dir,
            Path::new("/lower"),
            Path::new("/mount/target"),
        )
        .unwrap();

        // Metadata should be at wt_base level.
        let meta_path = wt_base.join(METADATA_FILENAME);
        assert!(meta_path.exists(), "metadata file should exist at wt_base");

        // Should NOT be inside snapshot_root.
        let wrong_path = snapshot_root.join(METADATA_FILENAME);
        assert!(
            !wrong_path.exists(),
            "metadata should not be in snapshot_root"
        );

        // Verify content round-trips.
        let content = std::fs::read_to_string(&meta_path).unwrap();
        let meta: OverlayMetadata = serde_json::from_str(&content).unwrap();
        assert_eq!(meta.kind, "overlay");
        assert_eq!(meta.snapshot_root, snapshot_root);
        assert_eq!(meta.mount_target, PathBuf::from("/mount/target"));
    }

    #[test]
    fn test_cleanup_orphaned_no_local_dir() {
        // Hosts without a `/local` overlay root should return an empty report.
        let report = cleanup_orphaned_overlay_snapshots();
        assert_eq!(report.removed, 0);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn test_metadata_scan_finds_matching_target() {
        // Verify metadata deserialization + match logic without full removal
        // (full removal needs a live btrfs host).
        let json = r#"{
            "type": "overlay",
            "snapshot_upper": "/var/lib/repo-fuse/instance/worktrees/wt1/upper",
            "work_dir": "/var/lib/repo-fuse/instance/worktrees/wt1/work",
            "lower_dir": "/var/lib/repo-fuse/instance/fuse-lower",
            "mount_target": "/home/user/.echo-agent/worktrees/wt1",
            "created_at": "1740000000s-since-epoch"
        }"#;
        let meta: OverlayMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.kind, "overlay");
        assert_eq!(
            meta.mount_target,
            PathBuf::from("/home/user/.echo-agent/worktrees/wt1")
        );
        // "snapshot_upper" in JSON maps to snapshot_root via serde alias
        assert_eq!(
            meta.snapshot_root,
            PathBuf::from("/var/lib/repo-fuse/instance/worktrees/wt1/upper")
        );
    }
}
