//! BTRFS filesystem and subvolume detection.
//!
//! This module handles detection of BTRFS filesystems and subvolumes, including
//! the case where a BTRFS subvolume is bind-mounted to another location (e.g.
//! when the working-tree path is not itself on BTRFS).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use nix::sys::statfs::{BTRFS_SUPER_MAGIC, statfs};

/// Information about a BTRFS subvolume.
#[derive(Debug, Clone)]
pub struct BtrfsInfo {
    /// Root path of the subvolume as seen by the user.
    /// This may be a bind mount target (e.g., `/workspace/repo`).
    pub subvolume_root: PathBuf,

    /// If the subvolume is accessed via a bind mount, this contains the actual
    /// source path on the btrfs filesystem (e.g., `/mnt/btrfs/repo`).
    /// None if the path is directly on btrfs without a bind mount.
    pub bind_mount_source: Option<PathBuf>,

    /// The btrfs mount point where snapshots can be created.
    /// For bind mounts, this is the parent of bind_mount_source.
    /// For direct btrfs paths, this is determined from the mount table.
    pub btrfs_mount_point: Option<PathBuf>,
}

/// Information about a bind mount.
#[derive(Debug, Clone)]
pub struct BindMountInfo {
    /// The target path (where the bind mount is visible)
    #[allow(dead_code)]
    pub target: PathBuf,
    /// The source path (actual location of the data)
    pub source: PathBuf,
    /// The filesystem type of the source
    pub fs_type: String,
}

/// Check if a path is on a BTRFS filesystem.
pub fn is_btrfs(path: &Path) -> Result<bool> {
    let stat = statfs(path).with_context(|| format!("statfs failed for {}", path.display()))?;
    Ok(stat.filesystem_type() == BTRFS_SUPER_MAGIC)
}

/// Get bind mount information for a path.
///
/// Uses `findmnt` to check if a path is a bind mount and retrieve its source.
/// Returns `Ok(Some(BindMountInfo))` if the path is a bind mount.
/// Returns `Ok(None)` if not a bind mount or if detection fails.
pub fn get_bind_mount_info(path: &Path) -> Result<Option<BindMountInfo>> {
    // Use findmnt to get mount information
    // -n: no headers, -o: output fields, -T: target path
    let mut cmd = Command::new("findmnt");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.stdin(Stdio::null());
    let output = cmd
        .args(["-n", "-o", "SOURCE,TARGET,FSTYPE,OPTIONS", "-T"])
        // OsStr arg: a non-UTF-8 path must not silently collapse to ".".
        .arg(path)
        .output();

    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            tracing::debug!(path = %path.display(), "findmnt failed or path not mounted");
            return Ok(None);
        }
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "failed to run findmnt");
            return Ok(None);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    if line.is_empty() {
        return Ok(None);
    }

    // Parse findmnt output: SOURCE TARGET FSTYPE OPTIONS
    // Fields are separated by whitespace
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        tracing::debug!(path = %path.display(), line = %line, "unexpected findmnt output format");
        return Ok(None);
    }

    let source = parts[0];
    let target = parts[1];
    let fs_type = parts[2];
    let options = parts.get(3).unwrap_or(&"");

    // Check if this is a bind mount by looking for "bind" in options
    // or by checking if source contains a subpath (e.g., /dev/loop0[/repo])
    let is_bind = options.contains("bind")
        || source.contains('[')
        || (source.starts_with('/') && !source.starts_with("/dev/"));

    if !is_bind {
        tracing::debug!(
            path = %path.display(),
            source = %source,
            "not a bind mount"
        );
        return Ok(None);
    }

    // For bind mounts, the source might be in format like "/dev/loop0[/repo]"
    // We need to resolve the actual path
    let actual_source = resolve_bind_mount_source(path)?;

    if let Some(actual_source) = actual_source {
        tracing::debug!(
            path = %path.display(),
            source = %actual_source.display(),
            "detected bind mount"
        );
        Ok(Some(BindMountInfo {
            target: PathBuf::from(target),
            source: actual_source,
            fs_type: fs_type.to_string(),
        }))
    } else {
        Ok(None)
    }
}

/// Resolve the actual source path for a bind mount by parsing /proc/self/mountinfo.
///
/// This is more reliable than parsing findmnt output for getting the actual path.
fn resolve_bind_mount_source(target: &Path) -> Result<Option<PathBuf>> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;

    let target_str = target.to_string_lossy();

    // Find the mount entry for our target
    // mountinfo format: ID PARENT_ID MAJOR:MINOR ROOT MOUNTPOINT OPTIONS - FSTYPE SOURCE SUPER_OPTIONS
    for line in mountinfo.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let mount_point = parts[4];
        if mount_point != target_str {
            continue;
        }

        // Found our mount point
        let root = parts[3]; // The root within the filesystem
        let fstype_idx = parts.iter().position(|&p| p == "-").map(|i| i + 1);

        if let Some(fstype_idx) = fstype_idx {
            let fstype = parts.get(fstype_idx).unwrap_or(&"");
            let source = parts.get(fstype_idx + 1).unwrap_or(&"");

            // For btrfs bind mounts, we need to find where the btrfs is mounted
            // and construct the full path
            if *fstype == "btrfs" {
                // Try 1: Find a btrfs root mount (root="/") for this device.
                // Common when the btrfs volume root is mounted separately
                // (e.g., `/mnt/btrfs/`).
                if let Some(btrfs_mount) = find_btrfs_mount_for_source(source, &mountinfo)? {
                    // Construct the full path: btrfs_mount + root
                    let full_source = if root == "/" {
                        btrfs_mount
                    } else {
                        btrfs_mount.join(root.trim_start_matches('/'))
                    };
                    return Ok(Some(full_source));
                }

                // Try 2: Resolve using a subvolume mount (no root mount exists).
                // This handles the case where only a btrfs subvolume is mounted
                // (e.g., `mount -o subvol=/repo /dev/loop0 /workspace/repo`)
                // without a separate mount for the btrfs volume root.
                match resolve_via_subvol_mount(source, root, &mountinfo) {
                    Ok(Some(full_source)) => return Ok(Some(full_source)),
                    Ok(None) => {
                        tracing::debug!(
                            device = %source,
                            root = %root,
                            target = %target_str,
                            "subvol mount fallback found no matching mount for btrfs device"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            device = %source,
                            root = %root,
                            target = %target_str,
                            error = %e,
                            "subvol mount fallback failed"
                        );
                    }
                }
            }
        }

        break;
    }

    Ok(None)
}

/// Find the mount point for a btrfs device/source.
fn find_btrfs_mount_for_source(source: &str, mountinfo: &str) -> Result<Option<PathBuf>> {
    // Look for a mount of this source that has root="/"
    for line in mountinfo.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let root = parts[3];
        let mount_point = parts[4];

        let fstype_idx = parts.iter().position(|&p| p == "-").map(|i| i + 1);
        if let Some(fstype_idx) = fstype_idx {
            let fstype = parts.get(fstype_idx).unwrap_or(&"");
            let mount_source = parts.get(fstype_idx + 1).unwrap_or(&"");

            if *fstype == "btrfs" && *mount_source == source && root == "/" {
                return Ok(Some(PathBuf::from(mount_point)));
            }
        }
    }

    Ok(None)
}

/// Resolve a btrfs path using an existing subvolume mount when no root mount exists.
///
/// On some hosts the btrfs volume root is not mounted separately — only a
/// specific subvolume is mounted (e.g.,
/// `mount -o subvol=/repo /dev/loop0 /workspace/repo`). In that case,
/// `find_btrfs_mount_for_source` returns `None` because there's no `root="/"` mount.
///
/// This function finds any mount of the same btrfs device and computes the filesystem
/// path by adjusting for the mount's root offset.
///
/// For example, with mount entry `root=/repo mount_point=/workspace/repo` and target
/// root `/repo/.echo-agent-snapshots/wt-123`, this returns
/// `/workspace/repo/.echo-agent-snapshots/wt-123`.
fn resolve_via_subvol_mount(
    device: &str,
    target_root: &str,
    mountinfo: &str,
) -> Result<Option<PathBuf>> {
    for line in mountinfo.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let mount_root = parts[3];
        let mount_point = parts[4];

        let fstype_idx = parts.iter().position(|&p| p == "-").map(|i| i + 1);
        if let Some(fstype_idx) = fstype_idx {
            let fstype = parts.get(fstype_idx).unwrap_or(&"");
            let mount_source = parts.get(fstype_idx + 1).unwrap_or(&"");

            if *fstype == "btrfs" && *mount_source == device {
                // Found a mount of this btrfs device.
                // Check if target_root starts with (or equals) this mount's root.
                if let Some(relative) = target_root.strip_prefix(mount_root) {
                    // Ensure we matched at a path boundary, not a partial name
                    // (e.g., mount_root="/repo" matches "/repo/foo" but not "/repo-other")
                    if relative.is_empty() || relative.starts_with('/') {
                        let relative = relative.trim_start_matches('/');
                        let full_source = if relative.is_empty() {
                            PathBuf::from(mount_point)
                        } else {
                            PathBuf::from(mount_point).join(relative)
                        };
                        return Ok(Some(full_source));
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Get the btrfs mount point that contains a given path.
///
/// This walks up the path hierarchy to find the nearest btrfs mount point.
pub fn get_btrfs_mount_point(path: &Path) -> Result<Option<PathBuf>> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;

    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Find the longest matching mount point that is btrfs
    let mut best_match: Option<PathBuf> = None;
    let mut best_len = 0;

    for line in mountinfo.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let mount_point = parts[4];
        let fstype_idx = parts.iter().position(|&p| p == "-").map(|i| i + 1);

        if let Some(fstype_idx) = fstype_idx {
            let fstype = parts.get(fstype_idx).unwrap_or(&"");

            if *fstype == "btrfs" && canonical.starts_with(mount_point) {
                let len = mount_point.len();
                if len > best_len {
                    best_len = len;
                    best_match = Some(PathBuf::from(mount_point));
                }
            }
        }
    }

    Ok(best_match)
}

/// Check if a path is a BTRFS subvolume and return info.
///
/// Returns `Ok(Some(BtrfsInfo))` if the path is a BTRFS subvolume root.
/// Returns `Ok(None)` if not on BTRFS or not a subvolume.
///
/// This function also detects bind-mounted BTRFS subvolumes. For example, if
/// `/workspace/repo` is bind-mounted from `/mnt/btrfs/repo`, this function will
/// detect it as a BTRFS subvolume and populate the `bind_mount_source` field.
///
/// Note: This checks if the path itself is a subvolume root, not if it's
/// contained within a subvolume.
pub fn is_btrfs_subvolume(path: &Path) -> Result<Option<BtrfsInfo>> {
    let on_btrfs = is_btrfs(path)?;

    if !on_btrfs {
        // Not on BTRFS at all (statfs says different fs type).
        // Check if it's a bind mount from a BTRFS subvolume anyway
        // (this handles the rare case where statfs doesn't report btrfs).
        tracing::debug!(
            path = %path.display(),
            "path not on BTRFS, checking for bind mount from BTRFS"
        );

        if let Some(bind_info) = get_bind_mount_info(path)?
            && bind_info.fs_type == "btrfs"
            && check_is_subvolume_cmd(&bind_info.source)
        {
            let btrfs_mount = get_btrfs_mount_point(&bind_info.source).ok().flatten();
            tracing::info!(
                path = %path.display(),
                source = %bind_info.source.display(),
                btrfs_mount = ?btrfs_mount,
                "path is a bind-mounted BTRFS subvolume"
            );
            return Ok(Some(BtrfsInfo {
                subvolume_root: path.to_path_buf(),
                bind_mount_source: Some(bind_info.source),
                btrfs_mount_point: btrfs_mount,
            }));
        }

        tracing::debug!(path = %path.display(), "not a BTRFS subvolume");
        return Ok(None);
    }

    // Path is on BTRFS (statfs reports btrfs). Check if it's a subvolume.
    if !check_is_subvolume_cmd(path) {
        tracing::debug!(
            path = %path.display(),
            "path is on BTRFS but not a subvolume root"
        );
        return Ok(None);
    }

    // It's a BTRFS subvolume. Now check if it's accessed via a bind mount.
    //
    // A path like `/workspace/repo` can be a bind mount FROM a btrfs subvolume
    // at `/mnt/btrfs/repo`. In that case, statfs reports btrfs (because the data
    // IS on btrfs), but the snapshot destination (e.g. `~/.echo-agent/worktrees/...`)
    // is NOT on btrfs. We need to detect the bind mount so we can create
    // snapshots inside the actual btrfs mount point and expose them at the
    // destination via a symlink.
    if let Some(bind_info) = get_bind_mount_info(path)?
        && bind_info.fs_type == "btrfs"
    {
        let btrfs_mount = get_btrfs_mount_point(&bind_info.source).ok().flatten();
        tracing::info!(
            path = %path.display(),
            source = %bind_info.source.display(),
            btrfs_mount = ?btrfs_mount,
            "path is a bind-mounted BTRFS subvolume"
        );
        return Ok(Some(BtrfsInfo {
            subvolume_root: path.to_path_buf(),
            bind_mount_source: Some(bind_info.source),
            btrfs_mount_point: btrfs_mount,
        }));
    }

    // Direct BTRFS subvolume (not bind-mounted)
    let btrfs_mount = get_btrfs_mount_point(path).ok().flatten();
    tracing::debug!(
        path = %path.display(),
        btrfs_mount = ?btrfs_mount,
        "path is a direct BTRFS subvolume"
    );
    Ok(Some(BtrfsInfo {
        subvolume_root: path.to_path_buf(),
        bind_mount_source: None,
        btrfs_mount_point: btrfs_mount,
    }))
}

/// Run `btrfs subvolume show` to check if a path is a subvolume.
fn check_is_subvolume_cmd(path: &Path) -> bool {
    let mut cmd = Command::new("btrfs");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.stdin(Stdio::null());
    // OsStr arg: a non-UTF-8 path must not silently collapse to ".".
    cmd.arg("subvolume")
        .arg("show")
        .arg(path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to check if we're running on a BTRFS filesystem.
    /// Returns the BTRFS path if available, None otherwise.
    fn get_btrfs_test_path() -> Option<PathBuf> {
        // Check environment variable first
        if let Ok(path) = std::env::var("BTRFS_TEST_PATH") {
            let path = PathBuf::from(path);
            if path.exists() && is_btrfs(&path).unwrap_or(false) {
                return Some(path);
            }
        }

        // Check common BTRFS mount points
        for candidate in &["/", "/home", "/btrfs", "/mnt/btrfs"] {
            let path = Path::new(candidate);
            if path.exists() && is_btrfs(path).unwrap_or(false) {
                return Some(path.to_path_buf());
            }
        }

        None
    }

    /// Helper to check if a path is a BTRFS subvolume for testing.
    fn get_btrfs_subvolume_test_path() -> Option<PathBuf> {
        if let Some(btrfs_path) = get_btrfs_test_path()
            && is_btrfs_subvolume(&btrfs_path).ok().flatten().is_some()
        {
            return Some(btrfs_path);
        }
        None
    }

    #[test]
    fn test_btrfs_info_debug() {
        let info = BtrfsInfo {
            subvolume_root: PathBuf::from("/test/path"),
            bind_mount_source: None,
            btrfs_mount_point: None,
        };
        let debug_str = format!("{:?}", info);
        assert!(debug_str.contains("BtrfsInfo"));
        assert!(debug_str.contains("/test/path"));
    }

    #[test]
    fn test_btrfs_info_clone() {
        let info = BtrfsInfo {
            subvolume_root: PathBuf::from("/original/path"),
            bind_mount_source: Some(PathBuf::from("/btrfs/source")),
            btrfs_mount_point: Some(PathBuf::from("/btrfs")),
        };
        let cloned = info.clone();
        assert_eq!(info.subvolume_root, cloned.subvolume_root);
        assert_eq!(info.bind_mount_source, cloned.bind_mount_source);
        assert_eq!(info.btrfs_mount_point, cloned.btrfs_mount_point);
    }

    #[test]
    fn test_btrfs_info_with_bind_mount() {
        let info = BtrfsInfo {
            subvolume_root: PathBuf::from("/workspace/repo"),
            bind_mount_source: Some(PathBuf::from("/mnt/btrfs/repo")),
            btrfs_mount_point: Some(PathBuf::from("/mnt/btrfs")),
        };
        assert!(info.bind_mount_source.is_some());
        assert_eq!(
            info.bind_mount_source.as_ref().unwrap(),
            &PathBuf::from("/mnt/btrfs/repo")
        );
        assert_eq!(
            info.btrfs_mount_point.as_ref().unwrap(),
            &PathBuf::from("/mnt/btrfs")
        );
    }

    #[test]
    fn test_is_btrfs_on_root() {
        // Test on root filesystem - should not panic regardless of fs type
        let result = is_btrfs(Path::new("/"));
        assert!(result.is_ok());
        // We can't assert the value since it depends on the system
    }

    #[test]
    fn test_is_btrfs_on_tmp() {
        // /tmp is typically not on BTRFS (tmpfs or ext4)
        // This test just verifies the function doesn't panic
        let result = is_btrfs(Path::new("/tmp"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_btrfs_nonexistent_path() {
        let result = is_btrfs(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("statfs failed"));
    }

    #[test]
    fn test_is_btrfs_subvolume_on_tmp() {
        // Should return None for non-BTRFS paths (or paths that aren't subvolumes)
        let result = is_btrfs_subvolume(Path::new("/tmp"));
        assert!(result.is_ok());
        // On most systems /tmp is not a BTRFS subvolume
        // But we can't assert None because some systems might have BTRFS /tmp
    }

    #[test]
    fn test_is_btrfs_subvolume_nonexistent_path() {
        // Nonexistent paths should error in is_btrfs before reaching btrfs command
        let result = is_btrfs_subvolume(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_err());
    }

    #[test]
    fn test_is_btrfs_subvolume_on_root() {
        // Test on root - should not panic
        let result = is_btrfs_subvolume(Path::new("/"));
        assert!(result.is_ok());
        // Result depends on whether / is a BTRFS subvolume
    }

    #[test]
    fn test_btrfs_detection_on_real_btrfs() {
        // This test automatically skips if no BTRFS is detected
        let Some(btrfs_path) = get_btrfs_test_path() else {
            eprintln!("Skipping test: no BTRFS filesystem detected");
            return;
        };

        let is_btrfs_result = is_btrfs(&btrfs_path);
        assert!(is_btrfs_result.is_ok());
        assert!(is_btrfs_result.unwrap(), "Expected path to be on BTRFS");
        eprintln!("BTRFS detected at: {}", btrfs_path.display());
    }

    #[test]
    fn test_btrfs_subvolume_detection_on_real_btrfs() {
        // This test automatically skips if no BTRFS subvolume is detected
        let Some(subvol_path) = get_btrfs_subvolume_test_path() else {
            eprintln!("Skipping test: no BTRFS subvolume detected");
            return;
        };

        let result = is_btrfs_subvolume(&subvol_path);
        assert!(result.is_ok());
        let info = result.unwrap();
        assert!(info.is_some(), "Expected path to be a BTRFS subvolume");
        eprintln!("BTRFS subvolume detected at: {}", subvol_path.display());
    }

    /// Test that a bind-mounted btrfs path (which reports as btrfs via statfs)
    /// correctly detects the bind mount and populates bind_mount_source.
    ///
    /// Regression: a bind-mounted working tree can be mis-detected as a "direct"
    /// btrfs subvolume (`bind_mount_source=None`), which then makes snapshot
    /// creation fail because the destination path is not on btrfs.
    ///
    /// Optional live check: set `BTRFS_BIND_TEST_PATH` to a bind-mounted btrfs
    /// path on the host. Skips when the env var is unset or the path is not a
    /// bind-mounted btrfs location.
    #[test]
    fn test_bind_mounted_btrfs_detects_bind_mount_source() {
        let path = match std::env::var_os("BTRFS_BIND_TEST_PATH") {
            Some(p) => PathBuf::from(p),
            None => {
                eprintln!("Skipping test: BTRFS_BIND_TEST_PATH not set");
                return;
            }
        };
        if !path.exists() {
            eprintln!(
                "Skipping test: BTRFS_BIND_TEST_PATH={} does not exist",
                path.display()
            );
            return;
        }

        // Check if it's on btrfs
        if !is_btrfs(&path).unwrap_or(false) {
            eprintln!("Skipping test: {} is not on btrfs", path.display());
            return;
        }

        // Check if it's a bind mount
        let bind_info = get_bind_mount_info(&path);
        let is_bind_mount = bind_info.as_ref().ok().and_then(|o| o.as_ref()).is_some();

        if !is_bind_mount {
            eprintln!("Skipping test: {} is not a bind mount", path.display());
            return;
        }

        // The critical test: is_btrfs_subvolume should detect the bind mount
        let result = is_btrfs_subvolume(&path);
        assert!(result.is_ok());
        let info = result.unwrap();
        assert!(
            info.is_some(),
            "Expected {} to be a BTRFS subvolume",
            path.display()
        );

        let info = info.unwrap();
        assert!(
            info.bind_mount_source.is_some(),
            "Expected bind_mount_source to be Some for bind-mounted btrfs path {}, \
             but got None. This would cause snapshot creation to fail because the destination \
             is not on btrfs.",
            path.display()
        );
        assert!(
            info.btrfs_mount_point.is_some(),
            "Expected btrfs_mount_point to be Some"
        );

        eprintln!(
            "Bind-mounted btrfs correctly detected:\n  path: {}\n  bind_source: {:?}\n  btrfs_mount: {:?}",
            info.subvolume_root.display(),
            info.bind_mount_source,
            info.btrfs_mount_point
        );
    }

    // ─── Unit tests for resolve_via_subvol_mount ─────────────────────────

    #[test]
    fn test_resolve_via_subvol_mount_exact_match() {
        // Simulates: mount -o subvol=/repo /dev/loop0 /workspace/repo
        // Target root is /repo (the subvolume itself)
        let mountinfo =
            "8267 8961 0:813 /repo /workspace/repo rw,relatime - btrfs /dev/loop0 rw,ssd";
        let result = resolve_via_subvol_mount("/dev/loop0", "/repo", mountinfo).unwrap();
        assert_eq!(result, Some(PathBuf::from("/workspace/repo")));
    }

    #[test]
    fn test_resolve_via_subvol_mount_nested_path() {
        // Simulates: target root /repo/.echo-agent-snapshots/wt-123 resolved via
        // mount with root=/repo at /workspace/repo
        let mountinfo =
            "8267 8961 0:813 /repo /workspace/repo rw,relatime - btrfs /dev/loop0 rw,ssd";
        let result = resolve_via_subvol_mount(
            "/dev/loop0",
            "/repo/.echo-agent-snapshots/wt-123",
            mountinfo,
        )
        .unwrap();
        assert_eq!(
            result,
            Some(PathBuf::from(
                "/workspace/repo/.echo-agent-snapshots/wt-123"
            ))
        );
    }

    #[test]
    fn test_resolve_via_subvol_mount_no_match() {
        // Target root doesn't start with mount root
        let mountinfo =
            "8267 8961 0:813 /repo /workspace/repo rw,relatime - btrfs /dev/loop0 rw,ssd";
        let result = resolve_via_subvol_mount("/dev/loop0", "/other", mountinfo).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_via_subvol_mount_partial_name_no_match() {
        // /repo-other should NOT match mount_root=/repo
        let mountinfo =
            "8267 8961 0:813 /repo /workspace/repo rw,relatime - btrfs /dev/loop0 rw,ssd";
        let result = resolve_via_subvol_mount("/dev/loop0", "/repo-other", mountinfo).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_via_subvol_mount_wrong_device() {
        // Different device should not match
        let mountinfo =
            "8267 8961 0:813 /repo /workspace/repo rw,relatime - btrfs /dev/loop0 rw,ssd";
        let result = resolve_via_subvol_mount("/dev/loop1", "/repo", mountinfo).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_via_subvol_mount_non_btrfs_ignored() {
        // ext4 mount should not match
        let mountinfo = "8970 8961 259:7 /mnt/local /local rw,relatime - ext4 /dev/nvme0n1p2 rw";
        let result = resolve_via_subvol_mount("/dev/nvme0n1p2", "/mnt/local", mountinfo).unwrap();
        assert_eq!(result, None);
    }
}
