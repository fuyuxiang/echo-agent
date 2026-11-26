//! Pure (I/O-free) patch application logic.
//!
//! Ported from `codex-rs/apply-patch/src/lib.rs`, but refactored so that
//! **no function in this module touches the filesystem**. Every function
//! accepts `&str` content directly, making the logic trivially testable.

use std::path::Path;

use super::errors::ApplyPatchError;
use super::parser::UpdateFileChunk;
use super::seek_sequence::seek_sequence;

/// Given the original file content as a `&str` and the list of update chunks,
/// compute and return the new file contents as a `String`.
///
/// This is the main entry point for the apply logic. It does NOT read from or
/// write to the filesystem.
pub fn derive_new_contents(
    original_content: &str,
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<String, ApplyPatchError> {
    let mut original_lines: Vec<String> = original_content.split('\n').map(String::from).collect();

    // Drop the trailing empty element that results from the final newline so
    // that line counts match the behaviour of standard `diff`.
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);

    // Ensure the file ends with a trailing newline.
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }

    Ok(new_lines.join("\n"))
}

/// Compute a list of replacements needed to transform `original_lines` into the
/// new lines, given the patch `chunks`. Each replacement is returned as
/// `(start_index, old_len, new_lines)`.
pub fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        // If a chunk has a `change_context`, use seek_sequence to find it,
        // then adjust our `line_index` to continue from there.
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
                line_index = idx + 1;
            } else {
                return Err(ApplyPatchError::ComputeReplacements(format!(
                    "Failed to find context '{}' in {}",
                    ctx_line,
                    path.display()
                )));
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure addition (no old lines). Add at the end or just before the
            // final empty line if one exists.
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        // Try to match the existing lines in the file with the old lines from
        // the chunk. If the pattern ends with a trailing empty string (final
        // newline), retry without it.
        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            return Err(ApplyPatchError::ComputeReplacements(format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
            )));
        }
    }

    replacements.sort_by_key(|(lhs_idx, _, _)| *lhs_idx);

    Ok(replacements)
}

/// Apply the `(start_index, old_len, new_lines)` replacements to
/// `original_lines`, returning the modified file contents as a vector of lines.
///
/// Replacements are applied in **reverse order** so that earlier replacements
/// don't shift the positions of later ones.
pub fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        let start_idx = *start_idx;
        let old_len = *old_len;

        // Remove old lines.
        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }

        // Insert new lines.
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start_idx + offset, new_line.clone());
        }
    }

    lines
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::implementations::codex::apply_patch::parser::{Hunk, parse_patch};

    /// Helper to construct a patch string with the given body.
    fn wrap_patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    #[test]
    fn test_update_file_modifies_content() {
        let original = "foo\nbar\n";
        let path = PathBuf::from("update.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@\n foo\n-bar\n+baz",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(result, "foo\nbaz\n");
    }

    #[test]
    fn test_multiple_chunks_in_single_file() {
        let original = "foo\nbar\nbaz\nqux\n";
        let path = PathBuf::from("multi.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@\n foo\n-bar\n+BAR\n@@\n baz\n-qux\n+QUX",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(result, "foo\nBAR\nbaz\nQUX\n");
    }

    #[test]
    fn test_interleaved_changes() {
        let original = "a\nb\nc\nd\ne\nf\n";
        let path = PathBuf::from("interleaved.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n\
             @@\n a\n-b\n+B\n\
             @@\n c\n d\n-e\n+E\n\
             @@\n f\n+g\n*** End of File",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(result, "a\nB\nc\nd\nE\nf\ng\n");
    }

    #[test]
    fn test_unicode_dash_matching() {
        // Original line contains EN DASH (\u{2013}) and NON-BREAKING HYPHEN (\u{2011}).
        let original = "import asyncio  # local import \u{2013} avoids top\u{2011}level dep\n";
        let path = PathBuf::from("unicode.py");
        // Patch uses plain ASCII dash / hyphen.
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n\
             @@\n\
             -import asyncio  # local import - avoids top-level dep\n\
             +import asyncio  # HELLO",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(result, "import asyncio  # HELLO\n");
    }

    #[test]
    fn test_pure_addition_then_removal() {
        let original = "line1\nline2\nline3\n";
        let path = PathBuf::from("panic.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n\
             @@\n+after-context\n+second-line\n\
             @@\n line1\n-line2\n-line3\n+line2-replacement",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(
            result,
            "line1\nline2-replacement\nafter-context\nsecond-line\n"
        );
    }

    #[test]
    fn test_context_not_found_returns_error() {
        let original = "foo\nbar\n";
        let path = PathBuf::from("missing.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@ nonexistent_context\n-bar\n+baz",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let err = derive_new_contents(original, &path, chunks).unwrap_err();
        match err {
            ApplyPatchError::ComputeReplacements(msg) => {
                assert!(msg.contains("Failed to find context"));
            }
            other => panic!("expected ComputeReplacements, got: {other:?}"),
        }
    }

    #[test]
    fn test_old_lines_not_found_returns_error() {
        let original = "foo\nbar\n";
        let path = PathBuf::from("mismatch.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@\n-nonexistent\n+replacement",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let err = derive_new_contents(original, &path, chunks).unwrap_err();
        match err {
            ApplyPatchError::ComputeReplacements(msg) => {
                assert!(msg.contains("Failed to find expected lines"));
            }
            other => panic!("expected ComputeReplacements, got: {other:?}"),
        }
    }

    #[test]
    fn test_end_of_file_marker_handling() {
        let original = "foo\nbar\nbaz\n";
        let path = PathBuf::from("eof.txt");
        let patch = wrap_patch(&format!(
            "*** Update File: {}\n@@\n+quux\n*** End of File",
            path.display()
        ));
        let parsed = parse_patch(&patch).unwrap();
        let chunks = match &parsed.hunks[0] {
            Hunk::UpdateFile { chunks, .. } => chunks,
            _ => panic!("expected UpdateFile"),
        };
        let result = derive_new_contents(original, &path, chunks).unwrap();
        assert_eq!(result, "foo\nbar\nbaz\nquux\n");
    }
}
