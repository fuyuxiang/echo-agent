//! Slice-mode reader — exact port of codex `slice::read()`.
//!
//! Reads lines from `offset` (1-indexed) up to `limit`, formatting each
//! as `L{line_number}: {content}`. Lines are truncated at `MAX_LINE_LENGTH`
//! at a char boundary.

/// Maximum number of characters per line before truncation.
pub(crate) const MAX_LINE_LENGTH: usize = 500;

/// Read a contiguous range of lines from file bytes in slice mode.
///
/// Returns formatted lines as `L{n}: {content}`, or an error string if
/// `offset` exceeds the number of lines in the file.
pub(crate) fn read_slice(
    file_bytes: &[u8],
    offset: usize,
    limit: usize,
) -> Result<Vec<String>, String> {
    let mut collected = Vec::new();
    let mut seen = 0usize;

    for raw_line in split_lines(file_bytes) {
        seen += 1;

        if seen < offset {
            continue;
        }
        if collected.len() == limit {
            break;
        }

        let formatted = format_line(raw_line);
        collected.push(format!("L{seen}: {formatted}"));

        if collected.len() == limit {
            break;
        }
    }

    if seen < offset {
        return Err("offset exceeds file length".to_string());
    }

    Ok(collected)
}

/// Format a raw byte line: decode as UTF-8 (lossy) and truncate at
/// `MAX_LINE_LENGTH` at a char boundary.
fn format_line(bytes: &[u8]) -> String {
    super::text_utils::format_display(bytes)
}

/// Split raw bytes into lines, stripping `\n` and `\r\n` line endings.
///
/// Every byte sequence separated by `\n` becomes a line. Trailing `\r`
/// on each line is also stripped. A final `\n` produces an empty trailing
/// entry (matching codex `BufReader::read_until(b'\n')` behavior).
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return vec![];
    }

    let mut lines = Vec::new();
    let mut start = 0;

    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            let mut end = i;
            // Strip trailing \r for \r\n endings.
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(&bytes[start..end]);
            start = i + 1;
        }
    }

    // Remaining bytes after the last \n (or all bytes if no \n found).
    if start < bytes.len() {
        let mut end = bytes.len();
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        lines.push(&bytes[start..end]);
    } else if start == bytes.len() && !bytes.is_empty() && bytes[bytes.len() - 1] == b'\n' {
        // File ends with \n — BufReader::read_until would NOT produce an
        // empty trailing line for this case. The codex implementation reads
        // until EOF and each read_until(b'\n') call consumes the delimiter.
        // A trailing \n means the last read produces the line before it;
        // no additional empty line is generated.
        // So we do NOT push an empty trailing entry here.
    }

    lines
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_requested_range() {
        let content = b"first\nsecond\nthird\nfourth\n";
        let result = read_slice(content, 2, 2).unwrap();
        assert_eq!(result, vec!["L2: second", "L3: third"]);
    }

    #[test]
    fn errors_when_offset_exceeds_length() {
        let content = b"one\ntwo\n";
        let result = read_slice(content, 100, 10);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "offset exceeds file length");
    }

    #[test]
    fn reads_non_utf8_lines() {
        let content = b"\xff\xfe\n";
        let result = read_slice(content, 1, 10).unwrap();
        assert_eq!(result.len(), 1);
        // Non-UTF8 bytes should be replaced with U+FFFD
        assert!(result[0].contains('\u{FFFD}'));
    }

    #[test]
    fn trims_crlf_endings() {
        let content = b"hello\r\nworld\r\n";
        let result = read_slice(content, 1, 10).unwrap();
        assert_eq!(result, vec!["L1: hello", "L2: world"]);
    }

    #[test]
    fn respects_limit_even_with_more_lines() {
        let content = b"a\nb\nc\nd\ne\n";
        let result = read_slice(content, 1, 3).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result, vec!["L1: a", "L2: b", "L3: c"]);
    }

    #[test]
    fn truncates_lines_longer_than_max_length() {
        let long_line = "x".repeat(600);
        let content = format!("{}\n", long_line);
        let result = read_slice(content.as_bytes(), 1, 10).unwrap();
        assert_eq!(result.len(), 1);
        // Line content should be truncated to MAX_LINE_LENGTH
        let expected_content = &long_line[..MAX_LINE_LENGTH];
        assert_eq!(result[0], format!("L1: {}", expected_content));
    }

    #[test]
    fn reads_single_line_no_trailing_newline() {
        let content = b"hello";
        let result = read_slice(content, 1, 10).unwrap();
        assert_eq!(result, vec!["L1: hello"]);
    }

    #[test]
    fn reads_from_offset_1() {
        let content = b"first\nsecond\nthird\n";
        let result = read_slice(content, 1, 10).unwrap();
        assert_eq!(result, vec!["L1: first", "L2: second", "L3: third"]);
    }

    #[test]
    fn empty_file_returns_error() {
        // Codex behavior: empty file has 0 lines, offset=1 exceeds file length.
        let content = b"";
        let result = read_slice(content, 1, 10);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "offset exceeds file length");
    }

    #[test]
    fn truncation_at_multibyte_char_boundary() {
        // Create a string that has multi-byte chars near the 500 boundary
        let mut s = "a".repeat(498);
        s.push('é'); // 2 bytes in UTF-8
        s.push('x');
        assert!(s.len() > MAX_LINE_LENGTH);
        let content = format!("{}\n", s);
        let result = read_slice(content.as_bytes(), 1, 10).unwrap();
        // The truncated line should be valid UTF-8 and <= MAX_LINE_LENGTH bytes
        let line_content = result[0].strip_prefix("L1: ").unwrap();
        assert!(line_content.len() <= MAX_LINE_LENGTH);
        assert!(line_content.is_char_boundary(line_content.len()));
    }
}
