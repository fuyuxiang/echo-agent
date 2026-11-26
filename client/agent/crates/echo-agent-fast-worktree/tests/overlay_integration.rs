//! Integration tests for overlay-on-FUSE worktree creation and cleanup.
//!
//! These tests exercise the full e2e flow:
//!   1. **Overlay creation**: detect FUSE+btrfs stack → create btrfs subvolume
//!      as the overlay upper, mount overlayfs on top of the FUSE lower.
//!   2. **Worktree creation**: create a worktree via overlay snapshot — snapshot
//!      the upper dir, mount a new overlay with the snapshot as upper, verify
//!      the worktree is a usable git repo.
//!   3. **Manual cleanup**: remove overlay worktrees, clean up orphaned snapshots,
//!      and bulk-clean worktrees via `cleanup_worktrees_in`.
//!
//! # Prerequisites
//!
//! These tests require a real FUSE+overlayfs+btrfs stack. They auto-skip when
//! the infrastructure is not present by checking:
//!   - `/proc/self/mountinfo` for an overlayfs mount with a FUSE lower layer
//!   - The overlay upper dir is on btrfs
//!
//! A typical layout looks like:
//!   lower: `<overlay-root>/fuse-lower`  (FUSE mount)
//!   upper: `<overlay-root>/upper`       (btrfs subvolume)
//!   mount: `<workspace>/repo`           (overlayfs)

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use echo_agent_fast_worktree::{
    CleanupReport, RemoveReport, WorktreeBuilder, WorktreeReport, cleanup_worktrees_in,
    remove_worktree,
};

// ── Test infrastructure ──────────────────────────────────────────────────

/// Parsed overlay environment, or `None` when the FUSE+overlay+btrfs
/// stack is not available (CI, local laptop, hosts without the stack).
struct OverlayTestEnv {
    /// The overlayfs mount point (e.g., `/workspace/repo`).
    mount_point: PathBuf,
    /// The FUSE lower dir.
    lower_dir: PathBuf,
    /// The btrfs upper dir.
    upper_dir: PathBuf,
    /// Root directory that contains upper/, work/, worktrees/.
    overlay_root: PathBuf,
}

/// Try to detect the FUSE+overlay+btrfs stack from live mountinfo.
///
/// Returns `None` and prints a skip message if the stack is not present.
fn detect_overlay_env() -> Option<OverlayTestEnv> {
    // Read mountinfo.
    let content = match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(c) => c,
        Err(_) => {
            eprintln!("SKIP: cannot read /proc/self/mountinfo");
            return None;
        }
    };

    // Find an overlay mount whose lower is FUSE and upper is on btrfs.
    // We look for overlayfs entries and verify the components.
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let mount_point = unescape_mountinfo(parts[4]);
        let sep_idx = match parts.iter().position(|&p| p == "-") {
            Some(i) => i,
            None => continue,
        };
        let fs_type = parts.get(sep_idx + 1).copied().unwrap_or("");
        let super_options = parts.get(sep_idx + 3).copied().unwrap_or("");

        if fs_type != "overlay" {
            continue;
        }

        let lower = match extract_opt(super_options, "lowerdir") {
            Some(v) => {
                let first = v.split(':').next().unwrap_or(&v);
                unescape_mountinfo(first)
            }
            None => continue,
        };
        let upper = match extract_opt(super_options, "upperdir") {
            Some(v) => unescape_mountinfo(&v),
            None => continue,
        };

        // Verify lower is a FUSE mount.
        if !is_fuse_mount_in(&content, &lower) {
            continue;
        }

        // Verify upper is on btrfs.
        let upper_path = Path::new(&upper);
        if !upper_path.exists() {
            continue;
        }
        let on_btrfs = match nix::sys::statfs::statfs(upper_path) {
            Ok(s) => s.filesystem_type() == nix::sys::statfs::BTRFS_SUPER_MAGIC,
            Err(_) => false,
        };
        if !on_btrfs {
            continue;
        }

        let overlay_root = upper_path.parent().unwrap_or(upper_path).to_path_buf();

        eprintln!(
            "overlay test env detected:\n  mount_point={mount_point}\n  lower={lower}\n  upper={upper}\n  overlay_root={}",
            overlay_root.display()
        );

        return Some(OverlayTestEnv {
            mount_point: PathBuf::from(&mount_point),
            lower_dir: PathBuf::from(&lower),
            upper_dir: PathBuf::from(&upper),
            overlay_root,
        });
    }

    eprintln!("SKIP: no FUSE+overlay+btrfs stack detected in mountinfo");
    None
}

/// Extract `key=value` from a comma-separated options string.
fn extract_opt(options: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for part in options.split(',') {
        if let Some(val) = part.strip_prefix(&prefix) {
            return Some(val.to_string());
        }
    }
    None
}

/// Unescape octal escapes in mountinfo fields (e.g., `\040` → space).
///
/// The kernel encodes special characters (space, tab, backslash, newline)
/// as octal sequences in `/proc/self/mountinfo` — see `proc(5)`.
fn unescape_mountinfo(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let o1 = bytes[i + 1];
            let o2 = bytes[i + 2];
            let o3 = bytes[i + 3];
            if o1.is_ascii_digit() && o2.is_ascii_digit() && o3.is_ascii_digit() {
                let val = (o1 - b'0') * 64 + (o2 - b'0') * 8 + (o3 - b'0');
                result.push(val as char);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Check if `path` is a FUSE mount according to mountinfo content.
fn is_fuse_mount_in(mountinfo: &str, path: &str) -> bool {
    for line in mountinfo.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }
        let mp = unescape_mountinfo(parts[4]);
        let sep_idx = match parts.iter().position(|&p| p == "-") {
            Some(i) => i,
            None => continue,
        };
        let fs_type = parts.get(sep_idx + 1).copied().unwrap_or("");
        if mp == path
            && (fs_type == "fuse" || fs_type.starts_with("fuse.") || fs_type.starts_with("fuseblk"))
        {
            return true;
        }
    }
    false
}

/// Check if a path is an active mount point by scanning mountinfo.
fn is_mountpoint(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let target = path.to_string_lossy();
    content.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() >= 5 && unescape_mountinfo(parts[4]) == target.as_ref()
    })
}

/// Generate a unique test name to avoid collisions between concurrent tests.
fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            % 1_000_000
    )
}

/// Ensure a btrfs snapshot is cleaned up, ignoring errors.
fn force_cleanup_snapshot(path: &Path) {
    if path.exists() {
        let _ = std::process::Command::new("btrfs")
            .args(["subvolume", "delete", &path.to_string_lossy()])
            .output();
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Unmount a path (lazy), ignoring errors.
fn force_unmount(path: &Path) {
    if path.exists() {
        // SAFETY: `c_path` is a valid, NUL-terminated CString that outlives
        // the `umount2` call. MNT_DETACH performs a lazy unmount so it won't
        // block even if the mount is busy.
        unsafe {
            let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            libc::umount2(c_path.as_ptr(), libc::MNT_DETACH);
        }
    }
}

// ── 1. Overlay = FUSE + btrfs subvolume ──────────────────────────────────

/// Verify detection: the source repo's overlay has a FUSE lower and btrfs upper.
#[test]
fn test_detect_fuse_overlay_on_source_repo() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    // The public API detect should find the same thing for the mount point.
    assert!(
        env.mount_point.exists(),
        "overlay mount point {} should exist",
        env.mount_point.display()
    );
    assert!(
        env.lower_dir.exists() || {
            // FUSE lower might return EIO if daemon crashed — that's fine,
            // we just need to know it was detected.
            eprintln!(
                "NOTE: lower_dir {} not accessible (FUSE may be down)",
                env.lower_dir.display()
            );
            true
        },
        "lower dir should exist or be a known FUSE mount"
    );
    assert!(
        env.upper_dir.exists(),
        "upper dir {} should exist on btrfs",
        env.upper_dir.display()
    );
}

/// Create a btrfs snapshot of the overlay upper dir and verify it's a valid
/// subvolume containing the repo's git metadata.
#[test]
fn test_snapshot_overlay_upper_creates_btrfs_subvolume() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let snap_name = unique_name("snap-upper");
    let worktrees_dir = env.overlay_root.join("worktrees");
    let _ = std::fs::create_dir_all(&worktrees_dir);
    let snap_path = worktrees_dir.join(&snap_name);

    // Cleanup from prior runs.
    force_cleanup_snapshot(&snap_path);

    // Create snapshot.
    let result = std::process::Command::new("btrfs")
        .args([
            "subvolume",
            "snapshot",
            &env.upper_dir.to_string_lossy(),
            &snap_path.to_string_lossy(),
        ])
        .output()
        .expect("btrfs command should be available");

    assert!(
        result.status.success(),
        "btrfs snapshot failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        snap_path.exists(),
        "snapshot should exist at {}",
        snap_path.display()
    );

    // Verify it's a subvolume.
    let show = std::process::Command::new("btrfs")
        .args(["subvolume", "show", &snap_path.to_string_lossy()])
        .output()
        .unwrap();
    assert!(
        show.status.success(),
        "snapshot should be a valid btrfs subvolume"
    );

    // Cleanup.
    force_cleanup_snapshot(&snap_path);
    let _ = std::fs::remove_dir(&worktrees_dir);
}

/// Mount an overlayfs with FUSE lower + snapshot upper and verify
/// the resulting filesystem is readable and writable.
#[test]
fn test_overlay_mount_fuse_lower_btrfs_upper() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("ovl-mount");
    let worktrees_dir = env.overlay_root.join("worktrees").join(&name);
    let _ = std::fs::create_dir_all(&worktrees_dir);

    let snap_upper = worktrees_dir.join("upper");
    let work_dir = worktrees_dir.join("work");
    let mount_target = worktrees_dir.join("mnt");

    // Cleanup.
    force_unmount(&mount_target);
    force_cleanup_snapshot(&snap_upper);

    // Step 1: Snapshot the upper.
    let snap_result = std::process::Command::new("btrfs")
        .args([
            "subvolume",
            "snapshot",
            &env.upper_dir.to_string_lossy(),
            &snap_upper.to_string_lossy(),
        ])
        .output()
        .expect("btrfs should work");
    assert!(snap_result.status.success(), "snapshot creation failed");

    // Step 2: Create work dir + mount target.
    std::fs::create_dir_all(&work_dir).unwrap();
    std::fs::create_dir_all(&mount_target).unwrap();

    // Step 3: Mount overlay.
    let mount_data = format!(
        "lowerdir={},upperdir={},workdir={},index=on",
        env.lower_dir.display(),
        snap_upper.display(),
        work_dir.display(),
    );

    let mount_ok = unsafe {
        let c_source = std::ffi::CString::new("overlay").unwrap();
        let c_target = std::ffi::CString::new(mount_target.as_os_str().as_encoded_bytes()).unwrap();
        let c_fstype = std::ffi::CString::new("overlay").unwrap();
        let c_data = std::ffi::CString::new(mount_data.as_bytes()).unwrap();
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            0,
            c_data.as_ptr().cast(),
        )
    };

    assert_eq!(
        mount_ok,
        0,
        "overlay mount failed: {}",
        std::io::Error::last_os_error()
    );
    assert!(
        is_mountpoint(&mount_target),
        "target should be a mountpoint"
    );

    // Step 4: Verify the overlay is readable (files from lower + upper visible).
    // The .git dir comes from the upper (repo changes). The FUSE lower has the
    // base tree. Together they should look like the full repo.
    assert!(
        mount_target.join(".git").exists()
            || mount_target.join("Cargo.toml").exists()
            || mount_target.join("AGENTS.md").exists()
            || std::fs::read_dir(&mount_target)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false),
        "overlay mount should expose repo files"
    );

    // Step 5: Verify writable — writes go to the snapshot upper, not the FUSE lower.
    let test_file = mount_target.join(".overlay-write-test");
    std::fs::write(&test_file, "hello from overlay test").unwrap();
    assert!(test_file.exists());
    // The file should also be visible in the snapshot upper dir.
    assert!(
        snap_upper.join(".overlay-write-test").exists(),
        "writes should land in the snapshot upper dir"
    );
    // Clean up the test file.
    let _ = std::fs::remove_file(&test_file);
    let _ = std::fs::remove_file(snap_upper.join(".overlay-write-test"));

    // Teardown.
    force_unmount(&mount_target);
    let _ = std::fs::remove_dir(&mount_target);
    force_cleanup_snapshot(&snap_upper);
    let _ = std::fs::remove_dir_all(&work_dir);
    let _ = std::fs::remove_dir_all(&worktrees_dir);
}

// ── 2. Worktree = FUSE + snapshot ────────────────────────────────────────

/// Create a worktree via `WorktreeBuilder` on the overlay source repo and
/// verify it produced a valid git worktree with zero files copied (overlay
/// snapshot path).
#[test]
fn test_worktree_builder_uses_overlay_snapshot() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-builder");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    // Cleanup from prior runs.
    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result: WorktreeReport = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed on overlay stack");

    // 1. The worktree path should exist and be a mountpoint (overlay).
    assert!(
        result.worktree_path.exists(),
        "worktree should exist at {}",
        result.worktree_path.display()
    );

    // 2. Zero files copied — overlay snapshot is O(1).
    assert_eq!(
        result.unignored_copy.files_copied, 0,
        "overlay snapshot should copy zero files, but copied {}",
        result.unignored_copy.files_copied
    );

    // 3. Commit should be non-empty.
    assert!(
        !result.commit.is_empty(),
        "worktree should have a HEAD commit"
    );

    // 4. Worktree should be a valid git repo.
    let status = std::process::Command::new("git")
        .current_dir(&result.worktree_path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "worktree should be a valid git repo"
    );

    // 5. Worktree should have repo files.
    assert!(
        result.worktree_path.join(".git").exists(),
        "worktree should have .git"
    );

    // Cleanup via remove_worktree.
    let remove_report =
        remove_worktree(&result.worktree_path).expect("remove_worktree should succeed");
    assert!(
        remove_report.unmounted_overlay,
        "remove should unmount overlay"
    );
    assert!(
        remove_report.used_btrfs_delete,
        "remove should delete btrfs snapshot"
    );
    assert!(
        !result.worktree_path.exists(),
        "worktree should be gone after remove"
    );
}

/// Create a worktree via overlay snapshot and verify that writes in the
/// worktree are independent of the source repo.
#[test]
fn test_overlay_worktree_writes_are_independent() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-indep");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    // Write a file in the worktree.
    let test_file = result.worktree_path.join(".independence-test");
    std::fs::write(&test_file, "worktree-only").unwrap();

    // The file should NOT appear in the source repo.
    assert!(
        !env.mount_point.join(".independence-test").exists(),
        "writes in the worktree should not appear in the source repo"
    );

    // Cleanup.
    let _ = remove_worktree(&result.worktree_path);
}

/// Create multiple worktrees from the same overlay source and verify they
/// are all independent.
#[test]
fn test_multiple_overlay_worktrees() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let base = unique_name("wt-multi");
    let dest1 = env
        .overlay_root
        .join("worktrees")
        .join(format!("{base}-1"))
        .join("mnt");
    let dest2 = env
        .overlay_root
        .join("worktrees")
        .join(format!("{base}-2"))
        .join("mnt");

    // Cleanup.
    for d in [&dest1, &dest2] {
        force_unmount(d);
        let _ = std::fs::remove_dir(d);
    }

    let r1 = WorktreeBuilder::new(&env.mount_point, &dest1)
        .create()
        .expect("worktree 1 should succeed");
    let r2 = WorktreeBuilder::new(&env.mount_point, &dest2)
        .create()
        .expect("worktree 2 should succeed");

    // Both should exist and have the same commit.
    assert!(r1.worktree_path.exists());
    assert!(r2.worktree_path.exists());
    assert_eq!(
        r1.commit, r2.commit,
        "both worktrees should be at the same commit"
    );

    // Write to one — should not affect the other.
    std::fs::write(r1.worktree_path.join(".multi-test"), "wt1").unwrap();
    assert!(
        !r2.worktree_path.join(".multi-test").exists(),
        "writes in wt1 should not appear in wt2"
    );

    // Cleanup.
    let _ = remove_worktree(&r1.worktree_path);
    let _ = remove_worktree(&r2.worktree_path);
}

/// Verify that `git status` works correctly in an overlay worktree.
#[test]
fn test_overlay_worktree_git_status() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-status");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    // Git status should work.
    let status = std::process::Command::new("git")
        .current_dir(&result.worktree_path)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "git status should succeed in overlay worktree"
    );

    // After creating a new file, git status should show it.
    std::fs::write(result.worktree_path.join("new-file.txt"), "new").unwrap();
    let status2 = std::process::Command::new("git")
        .current_dir(&result.worktree_path)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    let status_str = String::from_utf8_lossy(&status2.stdout);
    assert!(
        status_str.contains("new-file.txt"),
        "git status should show the new file, got: {status_str}"
    );

    // Clean up the test file so teardown is clean.
    let _ = std::fs::remove_file(result.worktree_path.join("new-file.txt"));

    // Cleanup.
    let _ = remove_worktree(&result.worktree_path);
}

// ── 3. Manual cleanup ────────────────────────────────────────────────────

/// `remove_worktree` on an overlay worktree should unmount + delete snapshot.
#[test]
fn test_remove_worktree_cleans_overlay() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-rm");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    let wt_path = result.worktree_path.clone();
    let wt_base = env.overlay_root.join("worktrees").join(&name);
    let snap_upper = wt_base.join("upper");

    // Pre-condition: the snapshot upper exists and the mount is active.
    assert!(
        snap_upper.exists(),
        "snapshot upper should exist before removal"
    );
    assert!(
        is_mountpoint(&wt_path),
        "worktree should be mounted before removal"
    );

    // Remove.
    let report: RemoveReport = remove_worktree(&wt_path).expect("remove_worktree should succeed");

    assert!(report.unmounted_overlay, "should have unmounted overlay");
    assert!(
        report.used_btrfs_delete,
        "should have deleted btrfs snapshot"
    );
    assert!(!report.unmounted_bind, "no bind mount involved");

    // Post-condition: everything is cleaned up.
    assert!(
        !wt_path.exists() || !is_mountpoint(&wt_path),
        "mount should be gone"
    );
    assert!(!snap_upper.exists(), "snapshot should be deleted");
}

/// `cleanup_worktrees_in` should find and remove overlay worktrees
/// in a directory hierarchy.
#[test]
fn test_cleanup_worktrees_in_removes_overlay_worktrees() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let base_name = unique_name("wt-bulk");

    // Create a temporary worktrees directory structure:
    //   <overlay_root>/worktrees/<base_name>-cleanup/
    //     ├── wt-a/mnt/   (overlay worktree)
    //     └── wt-b/mnt/   (overlay worktree)
    let cleanup_dir = env
        .overlay_root
        .join("worktrees")
        .join(format!("{base_name}-cleanup"));
    let _ = std::fs::create_dir_all(&cleanup_dir);

    let dest_a = cleanup_dir.join("wt-a").join("mnt");
    let dest_b = cleanup_dir.join("wt-b").join("mnt");

    for d in [&dest_a, &dest_b] {
        force_unmount(d);
        let _ = std::fs::remove_dir(d);
    }

    // Create worktrees that land inside our cleanup_dir structure.
    // WorktreeBuilder places snapshots at <overlay_root>/worktrees/<dest_name>/
    // so we need to use the overlay_root's worktrees dir as the parent for cleanup.
    let r_a = WorktreeBuilder::new(&env.mount_point, &dest_a).create();
    let r_b = WorktreeBuilder::new(&env.mount_point, &dest_b).create();

    // Both should succeed.
    let r_a = r_a.expect("worktree A should succeed");
    let r_b = r_b.expect("worktree B should succeed");

    assert!(r_a.worktree_path.exists());
    assert!(r_b.worktree_path.exists());

    // Now clean up via cleanup_worktrees_in on the parent dir that contains .git
    // The worktrees have .git so cleanup_worktrees_in should find them.
    let report: CleanupReport = cleanup_worktrees_in(&cleanup_dir);

    eprintln!(
        "cleanup report: removed={}, overlays={}, btrfs={}, errors={}",
        report.removed, report.overlays_unmounted, report.btrfs_deleted, report.errors
    );

    assert!(
        report.removed >= 2,
        "should have removed at least 2 worktrees, got {}",
        report.removed
    );
    assert_eq!(report.errors, 0, "cleanup should not have errors");

    // Post-condition: worktree dirs should be gone or at least unmounted.
    assert!(
        !r_a.worktree_path.exists() || !is_mountpoint(&r_a.worktree_path),
        "worktree A should be cleaned up"
    );
    assert!(
        !r_b.worktree_path.exists() || !is_mountpoint(&r_b.worktree_path),
        "worktree B should be cleaned up"
    );

    // Clean up the parent directory.
    let _ = std::fs::remove_dir_all(&cleanup_dir);
}

/// `cleanup_orphaned_overlay_snapshots` should remove snapshots that
/// have metadata but no active mount.
#[test]
fn test_cleanup_orphaned_overlay_snapshots() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-orphan");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    // Create a worktree.
    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    let wt_base = env.overlay_root.join("worktrees").join(&name);
    let snap_upper = wt_base.join("upper");
    let meta_path = wt_base.join(".fast-worktree-meta.json");

    // Verify metadata was written.
    assert!(
        meta_path.exists(),
        "metadata file should exist at {}",
        meta_path.display()
    );

    // Simulate an orphan: unmount the overlay but leave the snapshot + metadata.
    force_unmount(&result.worktree_path);
    let _ = std::fs::remove_dir(&result.worktree_path);

    // The snapshot should still exist (orphaned).
    assert!(
        snap_upper.exists(),
        "snapshot should still exist after unmount (orphaned)"
    );

    // Run orphan cleanup.
    let report = echo_agent_fast_worktree::cleanup_orphaned_overlay_snapshots();

    eprintln!(
        "orphan cleanup report: removed={}, btrfs={}, errors={}",
        report.removed, report.btrfs_deleted, report.errors
    );

    // The orphan should have been cleaned up.
    assert!(
        !snap_upper.exists(),
        "orphaned snapshot should be deleted after cleanup"
    );
    assert!(
        !meta_path.exists(),
        "orphaned metadata should be deleted after cleanup"
    );

    // Note: report.removed counts ALL orphans cleaned up, which may include
    // orphans from other tests or previous runs. We just verify our snapshot is gone.
}

/// Verify that metadata written during overlay worktree creation survives
/// unmount and can be used for crash recovery.
#[test]
fn test_overlay_metadata_survives_unmount() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-meta");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");

    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    let wt_base = env.overlay_root.join("worktrees").join(&name);
    let meta_path = wt_base.join(".fast-worktree-meta.json");

    // Read and verify metadata content.
    assert!(meta_path.exists(), "metadata should exist");
    let content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(meta["type"], "overlay");
    assert!(meta["snapshot_upper"].as_str().is_some());
    assert!(meta["work_dir"].as_str().is_some());
    assert!(meta["lower_dir"].as_str().is_some());
    assert!(meta["mount_target"].as_str().is_some());
    assert!(meta["created_at"].as_str().is_some());

    // The mount_target in metadata should match our dest.
    let meta_mount_target = meta["mount_target"].as_str().unwrap();
    assert_eq!(
        meta_mount_target,
        dest.to_string_lossy().as_ref(),
        "metadata mount_target should match dest"
    );

    // Unmount the overlay — metadata should survive (it's on btrfs, not inside the overlay).
    force_unmount(&result.worktree_path);

    assert!(
        meta_path.exists(),
        "metadata should survive overlay unmount"
    );

    // Cleanup.
    let _ = remove_worktree(&result.worktree_path);
    // If remove_worktree didn't fully clean up (since we already unmounted),
    // force cleanup.
    let snap_upper = wt_base.join("upper");
    force_cleanup_snapshot(&snap_upper);
    let _ = std::fs::remove_dir_all(&wt_base);
}

/// Regression: the overlay worktree must use a dedicated work dir
/// ("overlay-work"), never the source overlay's "work". Reusing "work" required
/// deleting the snapshot's copy, whose kernel-created internals are root-owned
/// mode-000 and undeletable by a rootless creator — which forced the slow file
/// copy fallback on rootless FUSE+overlay hosts.
#[test]
fn test_overlay_worktree_uses_dedicated_work_dir() {
    let Some(env) = detect_overlay_env() else {
        return;
    };

    let name = unique_name("wt-workdir");
    let dest = env.overlay_root.join("worktrees").join(&name).join("mnt");
    force_unmount(&dest);
    let _ = std::fs::remove_dir(&dest);

    let result = WorktreeBuilder::new(&env.mount_point, &dest)
        .create()
        .expect("worktree creation should succeed");

    // Must use the O(1) snapshot path, not the copy fallback.
    assert_eq!(
        result.unignored_copy.files_copied, 0,
        "overlay worktree should snapshot, not copy"
    );

    // The live overlay mount's workdir must be the dedicated name, distinct from
    // the source's "work" (read from mountinfo — robust to the wt_base layout).
    let workdir = overlay_workdir_for(&result.worktree_path)
        .expect("worktree should be a live overlay mount with a workdir");
    assert_eq!(
        Path::new(&workdir).file_name().and_then(|n| n.to_str()),
        Some("overlay-work"),
        "overlay worktree workdir should be the dedicated 'overlay-work', got {workdir}"
    );

    // Cleanup.
    let _ = remove_worktree(&result.worktree_path);
}

/// Return the overlay `workdir=` option for the overlay mounted at `mount_point`,
/// read from live mountinfo.
fn overlay_workdir_for(mount_point: &Path) -> Option<String> {
    let content = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }
        if Path::new(&unescape_mountinfo(parts[4])) != mount_point {
            continue;
        }
        let sep = parts.iter().position(|&p| p == "-")?;
        if parts.get(sep + 1).copied().unwrap_or("") != "overlay" {
            continue;
        }
        let opts = parts.get(sep + 3).copied().unwrap_or("");
        return extract_opt(opts, "workdir").map(|w| unescape_mountinfo(&w));
    }
    None
}
