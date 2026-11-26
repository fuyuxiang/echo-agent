//! Utilities for safe file reading with binary/UTF-8 detection.

use super::state::{FileContentState, MAX_TRACKED_TEXT_BYTES};

/// Git LFS pointer files start with this exact prefix.
/// See https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md
const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";

/// True when `bytes` is a Git LFS pointer stub.
///
/// LFS pointers are small text files (typically ~130 bytes) with the format:
/// ```text
/// version https://git-lfs.github.com/spec/v1
/// oid sha256:<hex>
/// size <digits>
/// ```
///
/// When the hunk tracker reads the raw git blob for an LFS-tracked file,
/// it gets this pointer text. The working copy, however, holds the real
/// (smudged) content. Detecting and marking LFS pointers prevents phantom
/// diffs that can never be resolved.
pub fn is_lfs_pointer(bytes: &[u8]) -> bool {
    // LFS pointers are always small (< 200 bytes typically).
    // Quick length check avoids scanning large buffers.
    bytes.len() < 1024 && bytes.starts_with(LFS_POINTER_PREFIX)
}

/// Check if content appears to be binary by looking for null bytes.
/// This is the same heuristic git uses.
pub fn is_binary(content: &[u8]) -> bool {
    // Check first 8000 bytes for null bytes (git's heuristic)
    let check_len = content.len().min(8000);
    content[..check_len].contains(&0)
}

/// Classify raw bytes into a FileContentState.
/// - Checks size BEFORE any allocation (bounded read guarantee)
/// - Checks for binary (null bytes in first 8KB) - no allocation needed
/// - Checks for valid UTF-8 only after size check passes
/// - Returns Full(String) only if all checks pass and size is within limit
pub fn classify_bytes(bytes: &[u8]) -> FileContentState {
    let byte_len = bytes.len();

    // Check size FIRST - before any allocation (MF-1: bounded read guarantee)
    if byte_len > MAX_TRACKED_TEXT_BYTES {
        return FileContentState::TooLarge { byte_len };
    }

    // LFS pointer check — operates on slice, no allocation.
    // Must come before the Full classification because LFS pointers are
    // valid UTF-8 text and would otherwise be returned as Full.
    if is_lfs_pointer(bytes) {
        return FileContentState::LfsPointer { byte_len };
    }

    // Binary check - operates on slice, no allocation
    if is_binary(bytes) {
        return FileContentState::Binary {
            byte_len: Some(byte_len),
        };
    }

    // Only now allocate the String (size is within limit)
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => FileContentState::Full(s),
        Err(_) => FileContentState::Binary {
            byte_len: Some(byte_len),
        },
    }
}

/// Classify a String into FileContentState based on size and binary content.
/// - Checks for NUL bytes (binary content) using the same heuristic as is_binary()
/// - Checks against MAX_TRACKED_TEXT_BYTES size limit
/// - Returns Full(String) only if size is within limit and content is text
pub fn classify_string(s: String) -> FileContentState {
    let byte_len = s.len();

    // Check size FIRST (matches classify_bytes order)
    if byte_len > MAX_TRACKED_TEXT_BYTES {
        return FileContentState::TooLarge { byte_len };
    }

    // LFS pointer check (same prefix test as classify_bytes)
    if is_lfs_pointer(s.as_bytes()) {
        return FileContentState::LfsPointer { byte_len };
    }

    // Check for binary content (NUL bytes in first 8KB)
    if is_binary(s.as_bytes()) {
        return FileContentState::Binary {
            byte_len: Some(byte_len),
        };
    }

    FileContentState::Full(s)
}

/// Create a FileContentState for a file that doesn't exist.
pub fn missing_content() -> FileContentState {
    FileContentState::Missing
}

/// Read a file and return FileContentState directly, with bounded allocation.
/// - Checks file metadata size BEFORE reading (MF-1: bounded read guarantee)
/// - Detects binary from a small prefix (8KB) without full read
/// - Returns TooLarge/Binary without allocating full content
/// - Only reads full content if within limit and text
pub async fn read_file_bounded(path: &std::path::Path) -> FileContentState {
    use tokio::io::AsyncReadExt;

    // Use symlink_metadata (lstat) to detect symlinks without following them.
    // Symlinks produce phantom diffs: git stores the target path string while
    // read() follows the link and returns the target file's content.
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(m) => m,
        Err(_) => return missing_content(),
    };
    if metadata.is_symlink() {
        return FileContentState::Symlink;
    }

    let byte_len = metadata.len() as usize;

    // Check size BEFORE any read (MF-1)
    if byte_len > MAX_TRACKED_TEXT_BYTES {
        return FileContentState::TooLarge { byte_len };
    }

    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return missing_content(),
    };

    // Read small prefix for binary detection (no full allocation)
    let prefix_size = 8000.min(byte_len);
    let mut prefix_buf = vec![0u8; prefix_size];
    let n = match file.read(&mut prefix_buf).await {
        Ok(n) => n,
        Err(_) => return missing_content(),
    };
    prefix_buf.truncate(n);

    // LFS pointer check on prefix (no full read needed — pointers are tiny)
    if is_lfs_pointer(&prefix_buf) {
        return FileContentState::LfsPointer { byte_len };
    }

    // Binary check on prefix only (no full read needed)
    if is_binary(&prefix_buf) {
        return FileContentState::Binary {
            byte_len: Some(byte_len),
        };
    }

    // Read remainder (total still within limit since we checked size upfront)
    let mut full_buf = prefix_buf;
    if byte_len > prefix_size && file.read_to_end(&mut full_buf).await.is_err() {
        return missing_content();
    }

    // Convert to String (size already checked)
    match String::from_utf8(full_buf) {
        Ok(s) => FileContentState::Full(s),
        Err(_) => FileContentState::Binary {
            byte_len: Some(byte_len),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_binary_with_null_byte() {
        let binary = b"hello\x00world";
        assert!(is_binary(binary));
    }

    #[test]
    fn test_is_binary_text_file() {
        let text = b"hello world\nthis is text\n";
        assert!(!is_binary(text));
    }

    #[test]
    fn test_is_binary_empty() {
        let empty: &[u8] = b"";
        assert!(!is_binary(empty));
    }

    // === TooLarge / bounded read tests (SF-2) ===

    #[test]
    fn test_classify_bytes_too_large() {
        // Create content larger than MAX_TRACKED_TEXT_BYTES
        let large = vec![b'a'; MAX_TRACKED_TEXT_BYTES + 1];
        let state = classify_bytes(&large);
        assert!(
            matches!(state, FileContentState::TooLarge { byte_len } if byte_len == MAX_TRACKED_TEXT_BYTES + 1)
        );
    }

    #[test]
    fn test_classify_bytes_too_large_with_null() {
        // Large content with null byte - should be TooLarge, not Binary
        // (size check happens first, so we short-circuit before binary check)
        let large: Vec<u8> = std::iter::repeat_n(b'a', MAX_TRACKED_TEXT_BYTES + 100).collect();
        let state = classify_bytes(&large);
        assert!(matches!(state, FileContentState::TooLarge { .. }));
    }

    #[test]
    fn test_classify_string_too_large() {
        let large = "a".repeat(MAX_TRACKED_TEXT_BYTES + 1);
        let state = classify_string(large);
        assert!(matches!(state, FileContentState::TooLarge { .. }));
    }

    #[test]
    fn test_classify_string_binary_with_null() {
        let binary = "hello\0world".to_string();
        let state = classify_string(binary);
        assert!(matches!(state, FileContentState::Binary { .. }));
    }

    #[tokio::test]
    async fn test_read_file_bounded_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let large = vec![b'a'; MAX_TRACKED_TEXT_BYTES + 1];
        std::fs::write(&path, &large).unwrap();
        let state = read_file_bounded(&path).await;
        assert!(
            matches!(state, FileContentState::TooLarge { byte_len } if byte_len == MAX_TRACKED_TEXT_BYTES + 1)
        );
    }

    // === LFS pointer tests ===

    #[test]
    fn test_is_lfs_pointer_valid() {
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 12345\n";
        assert!(is_lfs_pointer(pointer));
    }

    #[test]
    fn test_is_lfs_pointer_prefix_only() {
        let pointer = b"version https://git-lfs.github.com/spec/v1\n";
        assert!(is_lfs_pointer(pointer));
    }

    #[test]
    fn test_is_lfs_pointer_not_lfs() {
        let text = b"hello world\nthis is not an LFS pointer\n";
        assert!(!is_lfs_pointer(text));
    }

    #[test]
    fn test_is_lfs_pointer_empty() {
        assert!(!is_lfs_pointer(b""));
    }

    #[test]
    fn test_is_lfs_pointer_too_large_rejected() {
        // Even if content starts with LFS prefix, files >= 1024 bytes aren't pointers
        let mut large = b"version https://git-lfs.github.com/spec/v1\n".to_vec();
        large.resize(1024, b'x');
        assert!(!is_lfs_pointer(&large));
    }

    #[test]
    fn test_classify_bytes_lfs_pointer() {
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 12345\n";
        let state = classify_bytes(pointer);
        assert!(
            matches!(state, FileContentState::LfsPointer { byte_len } if byte_len == pointer.len())
        );
    }

    #[test]
    fn test_classify_string_lfs_pointer() {
        let pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 12345\n"
            .to_string();
        let len = pointer.len();
        let state = classify_string(pointer);
        assert!(matches!(state, FileContentState::LfsPointer { byte_len } if byte_len == len));
    }

    #[tokio::test]
    async fn test_read_file_bounded_lfs_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lfs_file.bin");
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc123\nsize 12345\n";
        std::fs::write(&path, pointer).unwrap();
        let state = read_file_bounded(&path).await;
        assert!(
            matches!(state, FileContentState::LfsPointer { byte_len } if byte_len == pointer.len())
        );
    }

    #[tokio::test]
    async fn test_read_file_bounded_binary_no_full_read() {
        // Binary file larger than limit - size check short-circuits first (TooLarge).
        // This is correct: no content retained either way, size is primary concern.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge_binary.bin");
        let mut data = vec![0xFFu8; MAX_TRACKED_TEXT_BYTES * 10];
        data[50] = 0; // null byte in prefix
        std::fs::write(&path, &data).unwrap();
        let state = read_file_bounded(&path).await;
        // Size > limit means TooLarge (bounded read guarantee - no full allocation)
        assert!(matches!(state, FileContentState::TooLarge { .. }));
    }

    #[tokio::test]
    async fn test_read_file_bounded_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.txt");
        std::fs::write(&target, "hello").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let state = read_file_bounded(&link).await;
        assert!(matches!(state, FileContentState::Symlink));
    }
}
