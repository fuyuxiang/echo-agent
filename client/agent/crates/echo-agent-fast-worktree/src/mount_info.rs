//! Shared `/proc/self/mountinfo` parser.
//!
//! Parses mountinfo once into structured `MountEntry` values, shared across
//! overlay and btrfs detection so we avoid duplicate parsing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A parsed entry from `/proc/self/mountinfo`.
///
/// Format per line:
/// ```text
/// ID PARENT MAJOR:MINOR ROOT MOUNTPOINT OPTIONS - FSTYPE SOURCE SUPER_OPTIONS
/// ```
#[derive(Debug, Clone)]
pub struct MountEntry {
    /// Mount ID.
    #[allow(dead_code)]
    pub mount_id: u32,
    /// Parent mount ID.
    #[allow(dead_code)]
    pub parent_id: u32,
    /// Root of the mount within the filesystem.
    #[allow(dead_code)]
    pub root: String,
    /// Mount point (where it's visible in the VFS).
    pub mount_point: PathBuf,
    /// Filesystem type (e.g., "overlay", "fuse.repo-fuse", "btrfs").
    pub fs_type: String,
    /// Mount source (device or special).
    #[allow(dead_code)]
    pub source: String,
    /// Super-block options (e.g., "lowerdir=...,upperdir=...,workdir=...").
    pub super_options: String,
}

/// Overlay-specific mount options parsed from `super_options`.
#[derive(Debug, Clone)]
pub struct OverlayMountInfo {
    /// The underlying `MountEntry`.
    pub entry: MountEntry,
    /// First (or only) lower directory.
    pub lower_dir: PathBuf,
    /// Upper directory.
    pub upper_dir: PathBuf,
    /// Work directory.
    pub work_dir: PathBuf,
}

/// Read and parse `/proc/self/mountinfo`.
pub fn parse_mountinfo() -> Result<Vec<MountEntry>> {
    let content =
        std::fs::read_to_string("/proc/self/mountinfo").context("read /proc/self/mountinfo")?;
    Ok(parse_mountinfo_from(&content))
}

/// Every overlay `upperdir` mounted across **all** namespaces, by scanning each
/// `/proc/<pid>/mountinfo`. Overlay worktrees may live in a different process's
/// mount namespace than the cleanup caller, so cleanup must check across
/// namespaces before deleting an overlay's backing snapshot. Unreadable entries
/// are skipped (a limited-visibility caller still sees its own namespace);
/// empty on platforms without `/proc`.
pub fn overlay_upperdirs_all_namespaces() -> std::collections::HashSet<PathBuf> {
    let mut uppers = std::collections::HashSet::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return uppers;
    };
    for proc in procs.flatten() {
        // Only numeric PID entries.
        if !proc
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(proc.path().join("mountinfo")) else {
            continue;
        };
        for entry in parse_mountinfo_from(&content) {
            if entry.fs_type == "overlay"
                && let Some(u) = extract_option(&entry.super_options, "upperdir")
            {
                // Unescape mountinfo octal escapes (e.g. `\040` → space) so the
                // stored upperdir matches the real filesystem paths callers
                // compare against (mirrors `find_overlay_mount`).
                uppers.insert(PathBuf::from(unescape_mountinfo(&u)));
            }
        }
    }
    uppers
}

/// Parse mountinfo from a string (testable without /proc).
pub fn parse_mountinfo_from(content: &str) -> Vec<MountEntry> {
    content.lines().filter_map(parse_line).collect()
}

/// Find the mount entry for `path` (longest mount_point prefix match).
#[allow(dead_code)]
pub fn find_mount_for_path<'a>(entries: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    let path_str = path.to_string_lossy();
    let mut best: Option<&MountEntry> = None;
    let mut best_len = 0;

    for entry in entries {
        let mp = entry.mount_point.to_string_lossy();
        if path_str.starts_with(mp.as_ref())
            && (path_str.len() == mp.len() || path_str.as_bytes().get(mp.len()) == Some(&b'/'))
            && mp.len() > best_len
        {
            best_len = mp.len();
            best = Some(entry);
        }
    }

    best
}

/// Find an overlay mount containing `path` and parse its options.
pub fn find_overlay_mount(entries: &[MountEntry], path: &Path) -> Option<OverlayMountInfo> {
    let path_str = path.to_string_lossy();
    let mut best: Option<&MountEntry> = None;
    let mut best_len = 0;

    for entry in entries {
        if entry.fs_type != "overlay" {
            continue;
        }
        let mp = entry.mount_point.to_string_lossy();
        if path_str.starts_with(mp.as_ref())
            && (path_str.len() == mp.len() || path_str.as_bytes().get(mp.len()) == Some(&b'/'))
            && mp.len() > best_len
        {
            best_len = mp.len();
            best = Some(entry);
        }
    }

    let entry = best?;
    let lower = extract_option(&entry.super_options, "lowerdir")?;
    let upper = extract_option(&entry.super_options, "upperdir")?;
    let work = extract_option(&entry.super_options, "workdir")?;

    // lowerdir can be colon-separated (multi-layer). Take the first one.
    let first_lower = lower.split(':').next().unwrap_or(&lower);

    Some(OverlayMountInfo {
        entry: entry.clone(),
        lower_dir: PathBuf::from(unescape_mountinfo(first_lower)),
        upper_dir: PathBuf::from(unescape_mountinfo(&upper)),
        work_dir: PathBuf::from(unescape_mountinfo(&work)),
    })
}

/// Result of comparing the current process's mount namespace with PID 1's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountNsStatus {
    /// Current process is in a mount namespace distinct from PID 1's.
    Private,
    /// Current process shares PID 1's mount namespace.
    Host,
    /// Could not determine — e.g. `/proc/1/ns/mnt` is unreadable for a non-root
    /// process (needs to own PID 1 or have `CAP_SYS_PTRACE`).
    Unknown,
}

/// Classify the current process's mount namespace relative to PID 1's.
///
/// Mounts created inside a private mount namespace (e.g. a container, a
/// `PrivateMounts=` systemd unit, or `unshare -m`) are invisible to processes
/// in other namespaces and are torn down when the namespace's last process
/// exits. Worktree strategies that materialize the worktree as a kernel mount
/// (bind mount, overlayfs) must avoid this so the worktree survives process
/// restart and is visible from the user's other shells.
///
/// Compares the mount-namespace identity of the current process
/// (`/proc/self/ns/mnt`) against PID 1 (`/proc/1/ns/mnt`). `Unknown` is returned
/// when either link is unreadable — most commonly a non-root process that can't
/// read `/proc/1/ns/mnt` (needs to own PID 1 or have `CAP_SYS_PTRACE`).
///
/// **Load-bearing assumption (see callers):** the only namespace-local strategy
/// gated on this is the overlay path; callers treat `Unknown` as *not* private
/// (overlay stays enabled) so a non-root caller on a normal host namespace is
/// not silently degraded to the slow copy path. This relies on environments
/// that actually exhibit the private-namespace issue typically running as
/// **root** (PID 1 readable → a genuine private namespace is detected as
/// `Private`). The btrfs-snapshot-symlink path is namespace-independent and
/// correct regardless of this classification; only the overlay (FUSE upper)
/// path could re-introduce an ephemeral worktree for a non-root process inside
/// a genuine private namespace — an accepted, documented residual.
pub fn current_mount_ns_status() -> MountNsStatus {
    let status = mount_ns_status(
        std::fs::read_link("/proc/self/ns/mnt"),
        std::fs::read_link("/proc/1/ns/mnt"),
    );
    if status == MountNsStatus::Unknown {
        log_unknown_mount_ns_once();
    }
    status
}

/// Decide mount-namespace status from the two `read_link` results.
///
/// Pure helper so the comparison/permission logic is unit-testable without
/// procfs.
fn mount_ns_status(
    self_ns: std::io::Result<PathBuf>,
    pid1_ns: std::io::Result<PathBuf>,
) -> MountNsStatus {
    match (self_ns, pid1_ns) {
        (Ok(self_ns), Ok(pid1_ns)) if self_ns == pid1_ns => MountNsStatus::Host,
        (Ok(_), Ok(_)) => MountNsStatus::Private,
        _ => MountNsStatus::Unknown,
    }
}

/// Emit a single diagnostic (across the process lifetime) noting that the
/// mount-namespace decision could not be made because `/proc/1/ns/mnt` was
/// unreadable, so namespace-local strategies remain enabled.
fn log_unknown_mount_ns_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        tracing::info!(
            "cannot read /proc/1/ns/mnt (likely non-root); treating mount namespace as \
             non-private — overlay/bind strategies stay enabled. If echoagent is in a private \
             namespace as non-root, worktrees may be ephemeral."
        );
    }
}

/// Check if `path` is a FUSE mount.
pub fn is_fuse_mount(entries: &[MountEntry], path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    entries.iter().any(|e| {
        e.mount_point.to_string_lossy() == path_str
            && (e.fs_type == "fuse"
                || e.fs_type.starts_with("fuse.")
                || e.fs_type.starts_with("fuseblk"))
    })
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Parse a single mountinfo line.
fn parse_line(line: &str) -> Option<MountEntry> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 10 {
        return None;
    }

    let mount_id = parts[0].parse::<u32>().ok()?;
    let parent_id = parts[1].parse::<u32>().ok()?;
    let root = parts[3].to_string();
    let mount_point = PathBuf::from(unescape_mountinfo(parts[4]));

    // Find the `-` separator.
    let sep_idx = parts.iter().position(|&p| p == "-")?;
    let fs_type = parts.get(sep_idx + 1)?.to_string();
    let source = parts.get(sep_idx + 2).unwrap_or(&"").to_string();
    let super_options = parts.get(sep_idx + 3).unwrap_or(&"").to_string();

    Some(MountEntry {
        mount_id,
        parent_id,
        root,
        mount_point,
        fs_type,
        source,
        super_options,
    })
}

/// Extract `key=value` from a comma-separated options string.
pub(crate) fn extract_option(options: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for part in options.split(',') {
        if let Some(val) = part.strip_prefix(&prefix) {
            return Some(val.to_string());
        }
    }
    None
}

/// Unescape octal escapes in mountinfo fields (e.g., `\040` → space).
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MOUNTINFO: &str = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw,errors=continue
50 22 0:44 / /var/lib/repo-fuse/instance/fuse-lower rw,nosuid,nodev,relatime - fuse.repo-fuse repo-fuse rw,user_id=0,group_id=0,allow_other
42 22 0:38 / /workspace/repo rw,relatime shared:2 - overlay overlay rw,lowerdir=/var/lib/repo-fuse/instance/fuse-lower,upperdir=/var/lib/repo-fuse/instance/upper,workdir=/var/lib/repo-fuse/instance/work,index=on
55 22 259:1 /btrfs-img /var/lib/repo-fuse/instance rw,relatime - btrfs /dev/loop0 rw,space_cache=v2,subvolid=256
";

    #[test]
    fn test_parse_mountinfo_entry_count() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn test_parse_mountinfo_overlay_entry() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let overlay = entries.iter().find(|e| e.fs_type == "overlay").unwrap();
        assert_eq!(overlay.mount_point, PathBuf::from("/workspace/repo"));
        assert!(overlay.super_options.contains("lowerdir="));
        assert!(overlay.super_options.contains("upperdir="));
    }

    #[test]
    fn test_parse_mountinfo_fuse_entry() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let fuse = entries
            .iter()
            .find(|e| e.fs_type.starts_with("fuse."))
            .unwrap();
        assert_eq!(
            fuse.mount_point,
            PathBuf::from("/var/lib/repo-fuse/instance/fuse-lower")
        );
        assert_eq!(fuse.fs_type, "fuse.repo-fuse");
    }

    #[test]
    fn test_find_mount_for_path() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let m = find_mount_for_path(&entries, Path::new("/workspace/repo")).unwrap();
        assert_eq!(m.fs_type, "overlay");

        let m2 = find_mount_for_path(&entries, Path::new("/workspace/repo/crates/foo")).unwrap();
        assert_eq!(m2.fs_type, "overlay");
    }

    #[test]
    fn test_find_overlay_mount() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let info = find_overlay_mount(&entries, Path::new("/workspace/repo")).unwrap();
        assert_eq!(
            info.lower_dir,
            PathBuf::from("/var/lib/repo-fuse/instance/fuse-lower")
        );
        assert_eq!(
            info.upper_dir,
            PathBuf::from("/var/lib/repo-fuse/instance/upper")
        );
        assert_eq!(
            info.work_dir,
            PathBuf::from("/var/lib/repo-fuse/instance/work")
        );
    }

    #[test]
    fn test_find_overlay_mount_subdirectory() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let info = find_overlay_mount(&entries, Path::new("/workspace/repo/crates/foo")).unwrap();
        assert_eq!(info.entry.mount_point, PathBuf::from("/workspace/repo"));
    }

    #[test]
    fn test_find_overlay_mount_none() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        assert!(find_overlay_mount(&entries, Path::new("/tmp")).is_none());
    }

    #[test]
    fn test_find_overlay_mount_path_boundary() {
        // /workspace/repo-extra should NOT match a mount at /workspace/repo.
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        assert!(
            find_overlay_mount(&entries, Path::new("/workspace/repo-extra")).is_none(),
            "/workspace/repo-extra should not match overlay at /workspace/repo"
        );
        // But /workspace/repo itself should match.
        assert!(find_overlay_mount(&entries, Path::new("/workspace/repo")).is_some());
        // And a proper subpath should match.
        assert!(find_overlay_mount(&entries, Path::new("/workspace/repo/foo")).is_some());
    }

    #[test]
    fn test_is_fuse_mount() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        assert!(is_fuse_mount(
            &entries,
            Path::new("/var/lib/repo-fuse/instance/fuse-lower")
        ));
        assert!(!is_fuse_mount(&entries, Path::new("/workspace/repo")));
        assert!(!is_fuse_mount(&entries, Path::new("/")));
    }

    #[test]
    fn test_extract_option() {
        let opts = "rw,lowerdir=/a/b,upperdir=/c/d,workdir=/e/f,index=on";
        assert_eq!(extract_option(opts, "lowerdir"), Some("/a/b".to_string()));
        assert_eq!(extract_option(opts, "upperdir"), Some("/c/d".to_string()));
        assert_eq!(extract_option(opts, "workdir"), Some("/e/f".to_string()));
        assert_eq!(extract_option(opts, "index"), Some("on".to_string()));
        assert_eq!(extract_option(opts, "missing"), None);
    }

    #[test]
    fn test_extract_option_multi_lower() {
        let opts = "rw,lowerdir=/a:/b:/c,upperdir=/d,workdir=/e";
        let lower = extract_option(opts, "lowerdir").unwrap();
        let first = lower.split(':').next().unwrap();
        assert_eq!(first, "/a");
    }

    #[test]
    fn test_unescape_mountinfo_no_escapes() {
        assert_eq!(unescape_mountinfo("/a/b/c"), "/a/b/c");
    }

    #[test]
    fn test_unescape_mountinfo_space() {
        assert_eq!(unescape_mountinfo("/a\\040b/c"), "/a b/c");
    }

    #[test]
    fn test_unescape_mountinfo_backslash() {
        // \134 is ASCII backslash
        assert_eq!(unescape_mountinfo("/a\\134b"), "/a\\b");
    }

    #[test]
    fn test_parse_mountinfo_ids() {
        let entries = parse_mountinfo_from(SAMPLE_MOUNTINFO);
        let root = entries
            .iter()
            .find(|e| e.mount_point == Path::new("/"))
            .unwrap();
        assert_eq!(root.mount_id, 22);
        assert_eq!(root.parent_id, 1);
    }

    #[test]
    fn test_parse_empty_mountinfo() {
        let entries = parse_mountinfo_from("");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_malformed_line() {
        let entries = parse_mountinfo_from("garbage data");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_mount_ns_status_distinct_links_is_private() {
        assert_eq!(
            mount_ns_status(
                Ok(PathBuf::from("mnt:[4026531840]")),
                Ok(PathBuf::from("mnt:[4026532998]")),
            ),
            MountNsStatus::Private
        );
    }

    #[test]
    fn test_mount_ns_status_identical_links_is_host() {
        assert_eq!(
            mount_ns_status(
                Ok(PathBuf::from("mnt:[4026531840]")),
                Ok(PathBuf::from("mnt:[4026531840]")),
            ),
            MountNsStatus::Host
        );
    }

    #[test]
    fn test_mount_ns_status_unreadable_is_unknown() {
        let perm = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        // The non-root case is pid1 unreadable; self-unreadable and both-unreadable
        // are also Unknown rather than Private.
        assert_eq!(
            mount_ns_status(Ok(PathBuf::from("mnt:[4026531840]")), Err(perm())),
            MountNsStatus::Unknown
        );
        assert_eq!(
            mount_ns_status(Err(perm()), Ok(PathBuf::from("mnt:[4026531840]"))),
            MountNsStatus::Unknown
        );
        assert_eq!(
            mount_ns_status(Err(perm()), Err(perm())),
            MountNsStatus::Unknown
        );
    }

    #[test]
    fn test_current_mount_ns_status_consistent() {
        assert_eq!(current_mount_ns_status(), current_mount_ns_status());
    }

    #[test]
    fn test_overlay_options_escaped_colons() {
        // Escaped colon in lowerdir path: /a\072b means /a:b
        let line =
            "42 1 0:38 / /mnt rw - overlay overlay rw,lowerdir=/a\\072b,upperdir=/u,workdir=/w";
        let entries = parse_mountinfo_from(line);
        let info = find_overlay_mount(&entries, Path::new("/mnt")).unwrap();
        assert_eq!(info.lower_dir, PathBuf::from("/a:b"));
    }
}
