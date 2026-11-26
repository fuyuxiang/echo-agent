//! Copy-on-Write file cloning using the reflink-copy crate.
//!
//! Uses reflink (CoW) when supported by the filesystem, with automatic
//! fallback to regular copy.

use std::path::Path;

use anyhow::Result;

/// Clone a file using CoW if supported, falling back to regular copy.
///
/// On filesystems that support it (APFS on macOS, Btrfs/XFS on Linux),
/// this creates a reflink which shares data blocks until modified.
/// On other filesystems, it performs a regular copy.
pub(crate) fn clone_file(src: &Path, dest: &Path) -> Result<()> {
    reflink_copy::reflink_or_copy(src, dest)?;
    // reflink (FICLONE) only clones data blocks, creating the dest with
    // default umask permissions. Explicitly propagate the source mode so the
    // executable bit etc. survive on reflink-capable filesystems.
    let perms = std::fs::metadata(src)?.permissions();
    std::fs::set_permissions(dest, perms)?;
    Ok(())
}

/// Recreate `dst` as a symlink pointing at `target`, replacing any existing
/// entry at `dst`.
///
/// `symlink()` refuses to overwrite an existing path, so we remove `dst` first
/// (a missing `dst` is not an error).
pub(crate) fn replace_symlink(target: &Path, dst: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(dst);
    symlink_to(target, dst)
}

#[cfg(unix)]
fn symlink_to(target: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, dst)
}

#[cfg(windows)]
fn symlink_to(target: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_clone_file_with_fallback() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        std::fs::write(&src, "hello world").unwrap();

        // Should work either via CoW or fallback
        clone_file(&src, &dest).unwrap();

        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello world");
    }

    #[test]
    fn test_clone_file_binary() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("source.bin");
        let dest = temp.path().join("dest.bin");

        let data: Vec<u8> = (0..=255).collect();
        std::fs::write(&src, &data).unwrap();

        clone_file(&src, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[test]
    fn test_clone_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let src = temp.path().join("script.sh");
        let dest = temp.path().join("script_copy.sh");

        std::fs::write(&src, "#!/bin/bash\necho hello").unwrap();

        // Make executable
        let mut perms = std::fs::metadata(&src).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&src, perms).unwrap();

        clone_file(&src, &dest).unwrap();

        let dest_perms = std::fs::metadata(&dest).unwrap().permissions();
        assert_eq!(dest_perms.mode() & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn test_replace_symlink_overwrites_and_allows_dangling() {
        let temp = TempDir::new().unwrap();
        let dst = temp.path().join("link");

        std::fs::write(&dst, "stale").unwrap();
        // Target is intentionally dangling; it must still be created.
        replace_symlink(Path::new("does-not-exist"), &dst).unwrap();

        let meta = std::fs::symlink_metadata(&dst).unwrap();
        assert!(meta.file_type().is_symlink(), "dst must be a symlink");
        assert_eq!(
            std::fs::read_link(&dst).unwrap(),
            Path::new("does-not-exist")
        );
    }
}
