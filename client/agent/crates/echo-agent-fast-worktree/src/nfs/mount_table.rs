//! Mount-table probes. macOS uses caller-owned `getfsstat` (never `getmntinfo`).
#[allow(unused_imports)]
use std::ffi::OsStr;
#[allow(unused_imports)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
#[allow(unused_imports)]
use std::path::PathBuf;
/// Result of a kernel mount-table lookup. A failed `getfsstat` / `mountinfo`
/// read is [`Inconclusive`], not [`NotMounted`].
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DestMountProbe {
    Mounted,
    NotMounted,
    Inconclusive,
}
/// True when `path` is a kernel mountpoint (any fstype).
#[must_use]
pub fn dest_is_mountpoint(path: &Path) -> bool {
    probe_dest_mount(path) == DestMountProbe::Mounted
}
/// True only when the mount table was readable and `path` is not a mountpoint.
/// Copy-fallback must use this, not `!dest_is_mountpoint`.
#[must_use]
pub fn dest_is_known_unmounted(path: &Path) -> bool {
    probe_dest_mount(path) == DestMountProbe::NotMounted
}
/// True when `path` is an NFS (nfs / nfs4 / nfsd) mountpoint.
#[must_use]
pub fn dest_is_nfs_mount(path: &Path) -> bool {
    mount_row_for(path).is_some_and(|(_, fstype)| is_nfs_fstype(&fstype))
}
/// True when `path` is a grove NFS or FUSE mount (source already projected).
#[must_use]
pub fn dest_is_projected_mount(path: &Path) -> bool {
    mount_row_for(path)
        .is_some_and(|(_, fstype)| is_nfs_fstype(&fstype) || is_grove_fuse_fstype(&fstype))
}
fn is_nfs_fstype(fstype: &str) -> bool {
    fstype == "nfs" || fstype == "nfs4" || fstype == "nfsd"
}
fn is_grove_fuse_fstype(fstype: &str) -> bool {
    fstype == "fuse.grove" || fstype == "fuse" || fstype.starts_with("fuse.")
}
/// Lexical dest compare. Never `canonicalize`: that stats every component and
/// can block indefinitely on a wedged NFS mount.
#[must_use]
pub(crate) fn dest_paths_equivalent(a: &Path, b: &Path) -> bool {
    paths_match(a, b)
}
/// Lexical `child` is `parent` or inside it, including macOS `/tmp`↔`/private/tmp`.
/// Never stats either path (wedged NFS dests hang `canonicalize`).
#[cfg_attr(not(any(test, feature = "metadata")), allow(dead_code))]
pub(crate) fn dest_path_contains(parent: &Path, child: &Path) -> bool {
    {
        if child.starts_with(parent) || paths_match(parent, child) {
            return true;
        }
        let p = normalize_mount_path(parent);
        let c = normalize_mount_path(child);
        c.starts_with(&p)
    }
}
#[cfg_attr(not(test), allow(dead_code))]
fn paths_match(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    normalize_mount_path(a) == normalize_mount_path(b)
}
#[cfg_attr(not(test), allow(dead_code))]
fn normalize_mount_path(p: &Path) -> PathBuf {
    let bytes = p.as_os_str().as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    let trimmed = PathBuf::from(OsStr::from_bytes(&bytes[..end]));
    #[cfg(target_os = "macos")]
    {
        let mut t = trimmed.to_string_lossy().into_owned();
        const DATA: &str = "/System/Volumes/Data";
        if t == DATA {
            t = "/".to_owned();
        } else if let Some(rest) = t.strip_prefix(DATA)
            && rest.starts_with('/')
        {
            t = rest.to_owned();
        }
        for (from, to) in [
            ("/tmp", "/private/tmp"),
            ("/var", "/private/var"),
            ("/etc", "/private/etc"),
        ] {
            if t == from {
                return PathBuf::from(to);
            }
            let prefix = format!("{from}/");
            if let Some(rest) = t.strip_prefix(&prefix) {
                return PathBuf::from(to).join(rest);
            }
        }
        PathBuf::from(t)
    }
    #[cfg(not(target_os = "macos"))]
    {
        trimmed
    }
}
pub(crate) fn probe_dest_mount(path: &Path) -> DestMountProbe {
    classify_mount_rows(path, read_mount_rows())
}
#[cfg_attr(not(test), allow(dead_code))]
fn classify_mount_rows(
    path: &Path,
    rows: std::io::Result<Vec<(String, String)>>,
) -> DestMountProbe {
    match rows {
        Err(_) => DestMountProbe::Inconclusive,
        Ok(rows)
            if rows
                .iter()
                .any(|(mnton, _)| paths_match(Path::new(mnton), path)) =>
        {
            DestMountProbe::Mounted
        }
        Ok(_) => DestMountProbe::NotMounted,
    }
}
fn mount_row_for(path: &Path) -> Option<(String, String)> {
    let rows = read_mount_rows().ok()?;
    rows.into_iter()
        .find(|(mnton, _)| paths_match(Path::new(mnton), path))
}
/// One `/proc/self/mountinfo` line → `(mountpoint, fstype)`.
/// Returns `None` for a malformed line so the caller can skip it.
#[cfg_attr(not(test), allow(dead_code))]
fn mountinfo_row(line: &str) -> Option<(String, String)> {
    let mut fields = line.split(' ');
    let mnton = fields.nth(4)?;
    let fstype = line.split(" - ").nth(1)?.split(' ').next()?;
    Some((unescape_mountinfo(mnton), fstype.to_owned()))
}
/// Kernel mountinfo encodes space/tab/newline/backslash as octal (`\040`).
#[cfg_attr(not(test), allow(dead_code))]
fn unescape_mountinfo(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &bytes[i + 1..i + 4];
            if oct.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let v = ((oct[0] - b'0') << 6) | ((oct[1] - b'0') << 3) | (oct[2] - b'0');
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn read_mount_rows() -> std::io::Result<Vec<(String, String)>> {
    #[cfg(target_os = "macos")]
    {
        read_mount_table_macos()
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/self/mountinfo")?;
        Ok(text.lines().filter_map(mountinfo_row).collect())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(Vec::new())
    }
}
#[cfg(target_os = "macos")]
fn read_mount_table_macos() -> std::io::Result<Vec<(String, String)>> {
    let flags = libc::MNT_NOWAIT;
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, flags) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let cap = (n as usize).saturating_add(8);
    let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; cap];
    let buf_bytes = (buf.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    let n2 = unsafe { libc::getfsstat(buf.as_mut_ptr(), buf_bytes, flags) };
    if n2 < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n2 as usize);
    Ok(buf
        .iter()
        .map(|st| (cstr_field(&st.f_mntonname), cstr_field(&st.f_fstypename)))
        .collect())
}
#[cfg(target_os = "macos")]
fn cstr_field(buf: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn plain_temp_dir_is_not_an_nfs_mount() {
        let tmp = TempDir::new().unwrap();
        assert!(!dest_is_nfs_mount(tmp.path()));
    }
    #[test]
    fn nonexistent_path_is_not_a_mountpoint() {
        assert!(!dest_is_mountpoint(Path::new(
            "/this/path/does/not/exist/nfs-pr13-probe"
        )));
        assert!(dest_is_known_unmounted(Path::new(
            "/this/path/does/not/exist/nfs-pr13-probe"
        )));
    }
    #[test]
    fn failed_mount_table_read_is_inconclusive_not_unmounted() {
        let err = std::io::Error::other("injected");
        assert_eq!(
            classify_mount_rows(Path::new("/mnt"), Err(err)),
            DestMountProbe::Inconclusive
        );
        assert_eq!(
            classify_mount_rows(Path::new("/mnt"), Ok(Vec::new())),
            DestMountProbe::NotMounted
        );
        assert_eq!(
            classify_mount_rows(Path::new("/mnt"), Ok(vec![("/mnt".into(), "nfs".into())])),
            DestMountProbe::Mounted
        );
    }
    #[test]
    fn malformed_mountinfo_line_is_skipped_not_fatal() {
        assert!(mountinfo_row("not-a-mountinfo-line").is_none());
        assert!(mountinfo_row("36 24 0:32 / /mnt rw shared:15").is_none());
        let ok = mountinfo_row(
            "36 24 0:32 / /mnt rw,relatime shared:15 - nfs 10.0.0.1:/export rw,vers=3",
        )
        .expect("well-formed mountinfo");
        assert_eq!(ok.0, "/mnt");
        assert_eq!(ok.1, "nfs");
    }
    #[test]
    fn mountinfo_octal_escapes_are_decoded() {
        let row = mountinfo_row(
            "36 24 0:32 / /mnt/my\\040wt rw,relatime shared:15 - nfs 10.0.0.1:/export rw,vers=3",
        )
        .expect("escaped mountpoint");
        assert_eq!(row.0, "/mnt/my wt");
        assert_eq!(row.1, "nfs");
        assert_eq!(
            unescape_mountinfo(r"/a\040b\011c\012d\134e"),
            "/a b\tc\nd\\e"
        );
    }
    #[test]
    fn dest_path_contains_is_lexical() {
        assert!(dest_path_contains(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/a/sub")
        ));
        assert!(dest_path_contains(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/a")
        ));
        assert!(!dest_path_contains(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/b")
        ));
        #[cfg(target_os = "macos")]
        assert!(dest_path_contains(
            Path::new("/tmp/nfs-wt"),
            Path::new("/private/tmp/nfs-wt/sub")
        ));
    }
    #[test]
    fn dest_paths_equivalent_is_lexical() {
        assert!(dest_paths_equivalent(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/a")
        ));
        assert!(dest_paths_equivalent(
            Path::new("/does/not/exist/a/"),
            Path::new("/does/not/exist/a")
        ));
        assert!(!dest_paths_equivalent(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/b")
        ));
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn dest_paths_equivalent_rewrites_macos_private_prefix() {
        assert!(dest_paths_equivalent(
            Path::new("/tmp/nfs-probe"),
            Path::new("/private/tmp/nfs-probe")
        ));
        assert!(dest_paths_equivalent(
            Path::new("/var/folders/xx/dest"),
            Path::new("/private/var/folders/xx/dest")
        ));
        assert!(!dest_paths_equivalent(
            Path::new("/variable/x"),
            Path::new("/private/var/iable/x")
        ));
        assert!(dest_paths_equivalent(
            Path::new("/Users/me/wt"),
            Path::new("/System/Volumes/Data/Users/me/wt")
        ));
        assert!(dest_paths_equivalent(
            Path::new("/tmp/nfs-probe"),
            Path::new("/System/Volumes/Data/private/tmp/nfs-probe")
        ));
    }
}
