//! Hash-based shard assignment for parallel file operations.

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rapidhash::v3::rapidhash_v3;

/// rapidhash of a path's raw bytes (lossy UTF-8 on non-unix).
fn rapidhash_path(path: &Path) -> u64 {
    #[cfg(unix)]
    let bytes = path.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let lossy = path.as_os_str().to_string_lossy();
    #[cfg(not(unix))]
    let bytes = lossy.as_bytes();
    rapidhash_v3(bytes)
}

/// Compute the shard index for a path based on its parent directory.
///
/// Files in the same directory will always be assigned to the same shard,
/// which avoids lock contention when creating parent directories.
pub(crate) fn shard_for_path(path: &Path, num_shards: usize) -> usize {
    let parent = path.parent().unwrap_or(path);
    (rapidhash_path(parent) as usize) % num_shards
}

/// Deterministic 16-hex-char (full 64-bit) hash of a path's full bytes.
///
/// Disambiguates same-basename worktrees that share a basename-derived key (btrfs
/// snapshot name, worktree DB id). Full 64 bits keep a collision astronomically
/// unlikely.
pub(crate) fn short_path_hash(path: &Path) -> String {
    format!("{:016x}", rapidhash_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_same_directory_same_shard() {
        let file1 = PathBuf::from("src/foo.rs");
        let file2 = PathBuf::from("src/bar.rs");
        let file3 = PathBuf::from("src/baz.rs");

        let num_shards = 8;

        let shard1 = shard_for_path(&file1, num_shards);
        let shard2 = shard_for_path(&file2, num_shards);
        let shard3 = shard_for_path(&file3, num_shards);

        // All files in src/ should go to the same shard
        assert_eq!(shard1, shard2);
        assert_eq!(shard2, shard3);
    }

    #[test]
    fn test_different_directories_may_differ() {
        let file1 = PathBuf::from("src/foo.rs");
        let file2 = PathBuf::from("tests/foo.rs");

        let num_shards = 8;

        // Different directories may (but don't have to) produce different shards
        let _shard1 = shard_for_path(&file1, num_shards);
        let _shard2 = shard_for_path(&file2, num_shards);
        // Just verify it doesn't panic
    }

    #[test]
    fn test_shard_in_range() {
        let path = PathBuf::from("some/deep/nested/path/file.txt");

        for num_shards in 1..=16 {
            let shard = shard_for_path(&path, num_shards);
            assert!(shard < num_shards);
        }
    }

    #[test]
    fn test_root_file() {
        let path = PathBuf::from("file.txt");
        let shard = shard_for_path(&path, 8);
        assert!(shard < 8);
    }
}
