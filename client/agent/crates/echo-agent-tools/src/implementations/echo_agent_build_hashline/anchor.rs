//! Anchor convenience helpers and re-exports.
//!
//! This module re-exports the core types from [`super::scheme`] and provides
//! helper functions for common anchor operations.

pub use super::scheme::{
    Anchor, AnchorScheme, CheckpointChain, ChunkFingerprint, ContentOnly, DEFAULT_SEARCH_RADIUS,
    ParsedAnchor, ShiftResult, ValidationResult,
};

/// Split file content into lines suitable for anchor generation.
///
/// Strips trailing newlines from each line (matching the convention used by
/// `AnchorScheme::generate_anchors`). The returned `Vec<&str>` has one entry
/// per logical line.
pub fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return vec![""];
    }

    let mut lines: Vec<&str> = content.lines().collect();

    // `str::lines()` does not yield a trailing empty entry for content ending
    // with '\n'. Add one to match the 1-based line numbering convention where
    // "hello\n" has 2 lines (line 1 = "hello", line 2 = "").
    if content.ends_with('\n') {
        lines.push("");
    }

    lines
}

/// Generate anchors for file content using the given scheme.
///
/// Convenience wrapper: splits `content` into lines and calls
/// `scheme.generate_anchors()`.
pub fn generate_for_content(scheme: &dyn AnchorScheme, content: &str) -> Vec<Anchor> {
    let lines = split_lines(content);
    scheme.generate_anchors(&lines)
}

/// Validate a parsed anchor against file content.
///
/// Convenience wrapper: splits `content` into lines and calls
/// `scheme.validate()`.
pub fn validate_against_content(
    scheme: &dyn AnchorScheme,
    anchor: &ParsedAnchor,
    content: &str,
) -> ValidationResult {
    let lines = split_lines(content);
    scheme.validate(anchor, &lines)
}

/// Search for a shifted anchor in file content.
///
/// Convenience wrapper: splits `content` into lines and calls
/// `scheme.find_shifted()`.
pub fn find_shifted_in_content(
    scheme: &dyn AnchorScheme,
    anchor: &ParsedAnchor,
    content: &str,
    search_radius: usize,
) -> ShiftResult {
    let lines = split_lines(content);
    scheme.find_shifted(anchor, &lines, search_radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_basic() {
        assert_eq!(split_lines("a\nb\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_lines_trailing_newline() {
        assert_eq!(split_lines("a\nb\n"), vec!["a", "b", ""]);
    }

    #[test]
    fn split_lines_empty() {
        assert_eq!(split_lines(""), vec![""]);
    }

    #[test]
    fn split_lines_single_newline() {
        assert_eq!(split_lines("\n"), vec!["", ""]);
    }

    #[test]
    fn generate_for_content_roundtrip() {
        let content = "line one\nline two\nline three\n";
        let scheme = ContentOnly::new();
        let anchors = generate_for_content(&scheme, content);
        assert_eq!(anchors.len(), 4); // 3 content lines + trailing empty
        assert_eq!(anchors[0].line, 1);
        assert_eq!(anchors[3].line, 4);
    }

    #[test]
    fn validate_against_content_valid() {
        let content = "let x = 1;\nlet y = 2;\n";
        let scheme = ContentOnly::new();
        let anchors = generate_for_content(&scheme, content);

        let parsed = ParsedAnchor {
            line: anchors[0].line,
            local: anchors[0].local.clone(),
            context: None,
        };
        assert_eq!(
            validate_against_content(&scheme, &parsed, content),
            ValidationResult::Valid
        );
    }

    #[test]
    fn validate_against_content_stale() {
        let original = "let x = 1;\nlet y = 2;\n";
        let scheme = ContentOnly::new();
        let anchors = generate_for_content(&scheme, original);

        let modified = "let x = 999;\nlet y = 2;\n";
        let parsed = ParsedAnchor {
            line: anchors[0].line,
            local: anchors[0].local.clone(),
            context: None,
        };
        assert_eq!(
            validate_against_content(&scheme, &parsed, modified),
            ValidationResult::Stale
        );
    }

    #[test]
    fn find_shifted_in_content_found() {
        let original = "a\nb\nc\n";
        let scheme = ContentOnly::new();
        let anchors = generate_for_content(&scheme, original);

        // Insert a line at the top → "b" shifts from line 2 to line 3.
        let modified = "new\na\nb\nc\n";
        let parsed = ParsedAnchor {
            line: anchors[1].line, // originally line 2 ("b")
            local: anchors[1].local.clone(),
            context: None,
        };

        match find_shifted_in_content(&scheme, &parsed, modified, 5) {
            ShiftResult::Found { new_line } => assert_eq!(new_line, 3),
            other => panic!("Expected Found, got {:?}", other),
        }
    }
}
