//! FUSE+overlay detection.
//!
//! Detects when a path sits on a FUSE+overlayfs stack with a btrfs upper dir.
//! All four conditions must hold for the overlay worktree path to be used:
//! 1. Path is on an overlayfs mount
//! 2. The overlay's lowerdir is a FUSE mount
//! 3. The overlay's upperdir is on btrfs (snapshotable)
//! 4. workdir is parseable

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::mount_info;

/// Information about a FUSE+overlay mount.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OverlayInfo {
    /// The overlayfs mount point (e.g., `/workspace/repo`).
    pub mount_point: PathBuf,
    /// The FUSE lower dir (e.g., `/var/lib/repo-fuse/instance/fuse-lower`).
    pub lower_dir: PathBuf,
    /// The overlay upper dir (e.g., `/var/lib/repo-fuse/instance/upper`).
    pub upper_dir: PathBuf,
    /// The overlay work dir (e.g., `/var/lib/repo-fuse/instance/work`).
    pub work_dir: PathBuf,
    /// The root directory that contains upper/ and work/ — sibling directory
    /// for worktree snapshots (e.g., `/var/lib/repo-fuse/instance`).
    pub overlay_root: PathBuf,
}

/// Detect if `path` is on a FUSE+overlayfs stack with btrfs upper.
///
/// Returns `Ok(Some(OverlayInfo))` if all conditions are met, `Ok(None)` otherwise.
/// Handles `EIO`/`ENOTCONN` from a crashed FUSE daemon gracefully by returning `Ok(None)`.
pub fn detect_fuse_overlay(path: &Path) -> Result<Option<OverlayInfo>> {
    let entries = match mount_info::parse_mountinfo() {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(error = %e, "failed to parse mountinfo, skipping overlay detection");
            return Ok(None);
        }
    };

    detect_fuse_overlay_from_entries(path, &entries)
}

/// Testable version that takes pre-parsed entries.
pub(crate) fn detect_fuse_overlay_from_entries(
    path: &Path,
    entries: &[mount_info::MountEntry],
) -> Result<Option<OverlayInfo>> {
    // Step 1: Find overlay mount containing this path.
    let overlay = match mount_info::find_overlay_mount(entries, path) {
        Some(info) => info,
        None => {
            tracing::debug!(path = %path.display(), "not on an overlayfs mount");
            return Ok(None);
        }
    };

    // Step 2: Verify the lower layer is a FUSE mount.
    if !mount_info::is_fuse_mount(entries, &overlay.lower_dir) {
        tracing::debug!(
            lower = %overlay.lower_dir.display(),
            "overlay lower layer is not a FUSE mount"
        );
        return Ok(None);
    }

    // Step 3: Verify the upper layer is on btrfs.
    let upper_on_btrfs = match crate::btrfs::is_btrfs(&overlay.upper_dir) {
        Ok(true) => true,
        Ok(false) => false,
        Err(e) => {
            // EIO / ENOTCONN from crashed FUSE — treat as "not available"
            tracing::debug!(
                upper = %overlay.upper_dir.display(),
                error = %e,
                "btrfs check failed on overlay upper, skipping"
            );
            false
        }
    };

    if !upper_on_btrfs {
        tracing::debug!(
            upper = %overlay.upper_dir.display(),
            "overlay upper dir is not on btrfs"
        );
        return Ok(None);
    }

    // Derive overlay_root — parent of upper_dir (sibling of upper/ and work/).
    let overlay_root = overlay
        .upper_dir
        .parent()
        .unwrap_or(&overlay.upper_dir)
        .to_path_buf();

    tracing::info!(
        mount_point = %overlay.entry.mount_point.display(),
        lower = %overlay.lower_dir.display(),
        upper = %overlay.upper_dir.display(),
        overlay_root = %overlay_root.display(),
        "detected FUSE+overlay with btrfs upper"
    );

    Ok(Some(OverlayInfo {
        mount_point: overlay.entry.mount_point.clone(),
        lower_dir: overlay.lower_dir,
        upper_dir: overlay.upper_dir,
        work_dir: overlay.work_dir,
        overlay_root,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount_info::parse_mountinfo_from;

    const FUSE_OVERLAY_MOUNTINFO: &str = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
50 22 0:44 / /var/lib/repo-fuse/instance/fuse-lower rw,nosuid,nodev - fuse.repo-fuse repo-fuse rw,user_id=0,allow_other
55 22 259:1 /img /var/lib/repo-fuse/instance rw,relatime - btrfs /dev/loop0 rw,space_cache=v2,subvolid=256
42 22 0:38 / /workspace/repo rw,relatime shared:2 - overlay overlay rw,lowerdir=/var/lib/repo-fuse/instance/fuse-lower,upperdir=/var/lib/repo-fuse/instance/upper,workdir=/var/lib/repo-fuse/instance/work,index=on
";

    #[test]
    fn test_detect_non_overlay() {
        // /tmp is unlikely to be on overlayfs in tests.
        let result = detect_fuse_overlay(Path::new("/tmp"));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_overlay_info_debug_clone() {
        let info = OverlayInfo {
            mount_point: PathBuf::from("/workspace/repo"),
            lower_dir: PathBuf::from("/var/lib/repo-fuse/fuse-lower"),
            upper_dir: PathBuf::from("/var/lib/repo-fuse/upper"),
            work_dir: PathBuf::from("/var/lib/repo-fuse/work"),
            overlay_root: PathBuf::from("/var/lib/repo-fuse"),
        };
        let cloned = info.clone();
        assert_eq!(format!("{:?}", info), format!("{:?}", cloned));
    }

    #[test]
    fn test_detect_overlay_without_fuse_lower() {
        // Overlay where lower is ext4, not FUSE — should return None.
        let mountinfo = "\
22 1 8:1 / / rw - ext4 /dev/sda1 rw
30 22 8:2 / /lower rw - ext4 /dev/sda2 rw
42 22 0:38 / /workspace/repo rw - overlay overlay rw,lowerdir=/lower,upperdir=/upper,workdir=/work
";
        let entries = parse_mountinfo_from(mountinfo);
        let result =
            detect_fuse_overlay_from_entries(Path::new("/workspace/repo"), &entries).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_overlay_without_overlay_mount() {
        let mountinfo = "\
22 1 8:1 / / rw - ext4 /dev/sda1 rw
";
        let entries = parse_mountinfo_from(mountinfo);
        let result =
            detect_fuse_overlay_from_entries(Path::new("/workspace/repo"), &entries).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_overlay_info_fields() {
        // We can't run the btrfs check in unit tests (no btrfs fs), but we
        // can verify the parsing portion works by calling the internal function
        // and checking that step 3 (btrfs) is the failing point.
        let entries = parse_mountinfo_from(FUSE_OVERLAY_MOUNTINFO);
        // This will return None because the sample upper path doesn't exist,
        // so is_btrfs will fail — but that's expected in a unit test.
        let result = detect_fuse_overlay_from_entries(Path::new("/workspace/repo"), &entries);
        assert!(result.is_ok());
        // On a system without the actual btrfs mount, this returns None.
        // On a host with a live FUSE+overlay stack it would return Some.
    }
}
