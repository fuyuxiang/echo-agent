//! Filesystem helpers shared across tool implementations.
//!
//! Thin wrappers around `tokio::fs` that add per-call tracing spans and a hard
//! timeout. The timeout guards against hung syscalls on slow or overlayfs-backed
//! filesystems (e.g. Docker overlay mounts), where `canonicalize` or `stat` can
//! block indefinitely. On timeout or error the helpers fall back to safe defaults
//! rather than propagating errors, keeping tool execution unblocked.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const FS_SYSCALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Async symlink-resolved path or the input path on failure/timeout.
///
/// Windows-safe canonicalizer: the result is passed through
/// `dunce::simplified` so Windows callers never see verbatim `\\?\` paths.
#[tracing::instrument(name = "fs.canonicalize", skip_all, fields(result))]
pub async fn canonicalize_with_timeout(path: PathBuf) -> PathBuf {
    // dunce-simplified below — blessed wrapper
    #[allow(clippy::disallowed_methods)]
    match tokio::time::timeout(FS_SYSCALL_TIMEOUT, tokio::fs::canonicalize(&path)).await {
        Ok(Ok(canonical)) => {
            tracing::Span::current().record("result", "ok");
            dunce::simplified(&canonical).to_path_buf()
        }
        Ok(Err(e)) => {
            tracing::Span::current().record("result", "error");
            tracing::debug!(error = %e, "canonicalize failed, using original path");
            path
        }
        Err(_elapsed) => {
            tracing::Span::current().record("result", "timeout");
            tracing::warn!(
                "canonicalize timed out after {}s (slow/overlayfs filesystem?), \
                 using original path",
                FS_SYSCALL_TIMEOUT.as_secs()
            );
            path
        }
    }
}

/// Async symlink-resolved path, preserving the `io::Error` on failure.
///
/// Error-preserving sibling of [`canonicalize_with_timeout`] for call sites
/// whose control flow branches on the `io::ErrorKind` (e.g. NotFound driving a
/// unicode-filename fallback or new-file creation), which the error-swallowing
/// helpers cannot express. Like the other blessed wrappers, the Ok result is
/// passed through `dunce::simplified` so Windows callers never see verbatim
/// `\\?\` paths. Deliberately no timeout: a synthetic TimedOut error would
/// change the `ErrorKind`-matching semantics at call sites.
pub(crate) async fn try_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    // dunce-simplified below — blessed wrapper
    #[allow(clippy::disallowed_methods)]
    tokio::fs::canonicalize(path)
        .await
        .map(|p| dunce::simplified(&p).to_path_buf())
}

/// OS-specific special characters that appear in generated filenames but that
/// models will never produce. Each entry maps a Unicode character to its ASCII
/// equivalent.
///
/// Separate from [`CONFUSABLE_MAP`] intentionally: CONFUSABLE_MAP is for file
/// *content* matching in `search_replace`, where characters like U+202F may be
/// legitimate. This map targets OS-generated filenames where the model can
/// never produce the exact character.
const FILENAME_SPECIAL_CHARACTER_MAP: &[(char, char)] = &[
    ('\u{202F}', ' '), // narrow no-break space (macOS screenshot/recording filenames)
    ('\u{00A0}', ' '), // no-break space
];

fn normalize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match FILENAME_SPECIAL_CHARACTER_MAP
            .iter()
            .find(|(from, _)| *from == c)
        {
            Some((_, replacement)) => out.push(*replacement),
            None => out.push(c),
        }
    }
    out
}

/// Result of a successful unicode-aware filename fallback resolution.
#[derive(Debug, Clone)]
pub struct UnicodePathMatch {
    /// The actual path on disk (with the original unicode characters).
    pub resolved_path: PathBuf,
    /// A note explaining what happened, suitable for appending to tool output.
    pub note: String,
}

/// When `path` does not exist, scan its parent directory for a file whose name
/// matches after normalizing unicode whitespace (e.g. U+202F → ASCII space).
///
/// macOS uses U+202F (narrow no-break space) before AM/PM in screenshot and
/// screen recording filenames. Models always produce regular U+0020 spaces,
/// so direct path lookups fail. This fallback bridges the gap.
///
/// Returns `None` if:
/// - the path already exists (caller should not have called this),
/// - the parent directory cannot be read,
/// - no entry matches after normalization,
/// - multiple entries match (ambiguous).
#[tracing::instrument(name = "fs.unicode_path_fallback", skip_all, fields(result))]
pub async fn try_resolve_unicode_filename(path: &Path) -> Option<UnicodePathMatch> {
    tokio::time::timeout(FS_SYSCALL_TIMEOUT, try_resolve_unicode_filename_inner(path))
        .await
        .unwrap_or_else(|_| {
            tracing::warn!(
                "unicode filename fallback timed out after {}s",
                FS_SYSCALL_TIMEOUT.as_secs()
            );
            None
        })
}

async fn try_resolve_unicode_filename_inner(path: &Path) -> Option<UnicodePathMatch> {
    let file_name = path.file_name()?.to_str()?;
    let parent = path.parent()?;

    let normalized_target = normalize_filename(file_name);

    let mut read_dir = tokio::fs::read_dir(parent).await.ok()?;

    let mut matches: Vec<PathBuf> = Vec::new();

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let entry_name = entry.file_name();
        let Some(entry_name_str) = entry_name.to_str() else {
            continue;
        };

        if entry_name_str == file_name {
            tracing::Span::current().record("result", "exact_match_exists");
            return None;
        }

        let normalized_entry = normalize_filename(entry_name_str);
        if normalized_entry == normalized_target {
            matches.push(entry.path());
        }
    }

    if matches.len() == 1 {
        let matched = &matches[0];
        let matched_name = matched.file_name().and_then(|n| n.to_str()).unwrap_or("?");

        // zip is safe: FILENAME_SPECIAL_CHARACTER_MAP is (char, char) so
        // every replacement preserves char count.
        let differing_chars: Vec<String> = matched_name
            .chars()
            .zip(file_name.chars())
            .filter(|(a, b)| a != b)
            .map(|(actual, _)| format!("U+{:04X}", actual as u32))
            .collect();

        let chars_list = if differing_chars.is_empty() {
            String::new()
        } else {
            format!(" ({})", differing_chars.join(", "))
        };

        let note = format!(
            "The specified filename did not exist exactly as given. A file was found \
             by normalizing Unicode characters{chars_list} to their ASCII equivalents. \
             The actual filename is: {matched_name}\n\
             For shell commands referencing this file, use glob patterns to avoid the mismatch.",
        );

        tracing::Span::current().record("result", "resolved");
        tracing::info!(
            original = %path.display(),
            resolved = %matched.display(),
            "unicode filename fallback resolved path"
        );

        Some(UnicodePathMatch {
            resolved_path: matched.clone(),
            note,
        })
    } else {
        let label = if matches.is_empty() {
            "no_match"
        } else {
            "ambiguous"
        };
        tracing::Span::current().record("result", label);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonicalize_falls_back_on_nonexistent_path() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let result = canonicalize_with_timeout(path.clone()).await;
        assert_eq!(result, path);
    }

    // ── try_resolve_unicode_filename ───────────────────────────────────

    #[tokio::test]
    async fn unicode_fallback_resolves_nnbsp_filename() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file with U+202F (narrow no-break space) before "PM"
        let actual_name = "Screenshot 2026-03-20 at 12.37.23\u{202F}PM.png";
        let actual_path = dir.path().join(actual_name);
        tokio::fs::write(&actual_path, b"img").await.unwrap();

        // Model provides the same name with regular space
        let model_name = "Screenshot 2026-03-20 at 12.37.23 PM.png";
        let model_path = dir.path().join(model_name);

        let result = try_resolve_unicode_filename(&model_path).await;
        assert!(result.is_some(), "should resolve via unicode fallback");
        let m = result.unwrap();
        assert_eq!(m.resolved_path, actual_path);
        assert!(m.note.contains("U+202F"));
        assert!(m.note.contains(actual_name));
    }

    #[tokio::test]
    async fn unicode_fallback_resolves_nbsp_filename() {
        let dir = tempfile::tempdir().unwrap();
        let actual_name = "doc\u{00A0}final.txt";
        let actual_path = dir.path().join(actual_name);
        tokio::fs::write(&actual_path, b"txt").await.unwrap();

        let model_name = "doc final.txt";
        let model_path = dir.path().join(model_name);

        let result = try_resolve_unicode_filename(&model_path).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().resolved_path, actual_path);
    }

    #[tokio::test]
    async fn unicode_fallback_returns_none_for_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        let name = "normal file.txt";
        let path = dir.path().join(name);
        tokio::fs::write(&path, b"ok").await.unwrap();

        let result = try_resolve_unicode_filename(&path).await;
        assert!(result.is_none(), "exact match should return None");
    }

    #[tokio::test]
    async fn unicode_fallback_returns_none_for_no_match() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("other.txt"), b"x")
            .await
            .unwrap();

        let model_path = dir.path().join("nonexistent.txt");
        let result = try_resolve_unicode_filename(&model_path).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn unicode_fallback_returns_none_for_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        // Two files that normalize to the same ASCII name
        let a = dir.path().join("file\u{202F}name.txt");
        let b = dir.path().join("file\u{00A0}name.txt");
        tokio::fs::write(&a, b"a").await.unwrap();
        tokio::fs::write(&b, b"b").await.unwrap();

        let model_path = dir.path().join("file name.txt");
        let result = try_resolve_unicode_filename(&model_path).await;
        assert!(result.is_none(), "ambiguous matches should return None");
    }

    #[tokio::test]
    async fn unicode_fallback_returns_none_for_nonexistent_parent() {
        let path = PathBuf::from("/nonexistent/dir/Screenshot 2026-03-20 at 12.37.23 PM.png");
        let result = try_resolve_unicode_filename(&path).await;
        assert!(result.is_none());
    }
}
