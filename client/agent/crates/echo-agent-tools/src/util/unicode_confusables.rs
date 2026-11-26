//! Unicode confusable-character detection and normalization.
//!
//! Several Unicode punctuation characters are visually indistinguishable from
//! their ASCII counterparts in most terminal and editor fonts.  When a file
//! contains these characters (e.g., text pasted from Slack, Notion, or Google
//! Docs), `read_file` renders them identically to ASCII, but `search_replace`
//! performs exact byte matching and therefore fails to find the model-supplied
//! ASCII `old_string`.
//!
//! This module provides a **narrow, typography-focused** confusable map and
//! helpers for:
//!
//! - detecting whether a string contains confusable characters,
//! - normalizing confusables to their ASCII equivalents (for comparison only),
//! - locating confusable characters with byte offsets and line numbers,
//! - building a byte-offset remapping table so that match positions found in a
//!   normalized string can be translated back to the original byte positions.
//!
//! ## Design constraints
//!
//! The confusable set is intentionally small: only high-confidence typography
//! substitutions that are almost always accidental (smart quotes, dashes,
//! ellipsis, non-breaking space).  Characters like `U+2212` (minus sign) or
//! `U+00D7` (multiplication sign) are excluded because they can carry semantic
//! meaning.

/// Narrow, typography-focused map of visually confusable Unicode characters.
///
/// Each entry maps a Unicode character to its ASCII equivalent string.
/// The replacement may be one or more ASCII characters (e.g., em-dash → `"--"`).
///
/// This list is intentionally conservative.  Additions should be limited to
/// characters that are (a) visually identical to ASCII in monospace fonts and
/// (b) almost always produced by accidental rich-text auto-correction rather
/// than deliberate content authoring.
pub const CONFUSABLE_MAP: &[(char, &str)] = &[
    ('\u{201C}', "\""),  // " left double quotation mark
    ('\u{201D}', "\""),  // " right double quotation mark
    ('\u{2018}', "'"),   // ' left single quotation mark
    ('\u{2019}', "'"),   // ' right single quotation mark
    ('\u{2014}', "--"),  // — em-dash
    ('\u{2013}', "-"),   // – en-dash
    ('\u{2026}', "..."), // … horizontal ellipsis
    ('\u{00A0}', " "),   // non-breaking space
];

/// A single detected confusable character with its location metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfusableHit {
    /// Byte offset of the confusable character in the source string.
    pub byte_offset: usize,
    /// The Unicode character that was detected.
    pub unicode_char: char,
    /// The ASCII replacement string from [`CONFUSABLE_MAP`].
    pub ascii_replacement: &'static str,
    /// 1-based line number where the character appears.
    pub line_number: usize,
}

/// Look up a character in the confusable map.
///
/// Returns the ASCII replacement string if `c` is a known confusable, or
/// `None` otherwise.  This is an O(n) scan over the (small, constant-size)
/// map; a `HashMap` would add startup cost and an external dependency for
/// negligible gain given the current map size.
fn lookup(c: char) -> Option<&'static str> {
    CONFUSABLE_MAP
        .iter()
        .find(|&&(ch, _)| ch == c)
        .map(|&(_, replacement)| replacement)
}

/// Fast check: does `s` contain any character from [`CONFUSABLE_MAP`]?
///
/// Returns as soon as the first confusable is found (short-circuiting).
pub fn has_confusables(s: &str) -> bool {
    s.chars().any(|c| lookup(c).is_some())
}

/// Replace every occurrence of a [`CONFUSABLE_MAP`] character with its ASCII
/// equivalent.
///
/// Characters not in the map are copied through unchanged (including non-ASCII
/// characters such as emoji or CJK that are not in the map).
///
/// If the input contains no confusables, this allocates a new `String` with
/// identical content.  Callers that want to avoid allocation on the common case
/// should check [`has_confusables`] first.
pub fn normalize_confusables(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match lookup(c) {
            Some(replacement) => out.push_str(replacement),
            None => out.push(c),
        }
    }
    out
}

/// Detect all confusable characters in `s`, returning their positions and
/// line numbers.
///
/// Results are ordered by ascending `byte_offset`.  Line numbers are 1-based.
pub fn detect_confusables(s: &str) -> Vec<ConfusableHit> {
    let mut hits = Vec::new();
    let mut line: usize = 1;
    for (byte_offset, c) in s.char_indices() {
        if let Some(replacement) = lookup(c) {
            hits.push(ConfusableHit {
                byte_offset,
                unicode_char: c,
                ascii_replacement: replacement,
                line_number: line,
            });
        }
        if c == '\n' {
            line += 1;
        }
    }
    hits
}

/// Build a normalized string together with a byte-offset mapping from the
/// normalized string back to the original.
///
/// Returns `(normalized_text, offset_map)` where:
///
/// - `normalized_text` is the result of applying [`normalize_confusables`] to
///   `s`.
/// - `offset_map` has length `normalized_text.len() + 1`.  For every byte
///   index `i` in `0..=normalized_text.len()`, `offset_map[i]` is the
///   corresponding byte index in the original string `s`.
///
/// The **terminal sentinel** at `offset_map[normalized_text.len()]` equals
/// `s.len()`, ensuring that a normalized match ending exactly at the end of
/// the string can be safely remapped.
///
/// # Boundary-mapping contract (for substring remapping)
///
/// The primary consumer of this function is normalized-fallback matching.
/// The intended usage pattern is:
///
/// 1. Build `(normalized_text, offset_map)` from the file content.
/// 2. Normalize the search pattern with [`normalize_confusables`].
/// 3. Find a match at `[norm_start..norm_end]` in `normalized_text`.
/// 4. Recover the corresponding original byte span:
///    ```text
///    original_start = offset_map[norm_start]
///    original_end   = offset_map[norm_end]
///    original_slice = &s[original_start..original_end]
///    ```
/// 5. The recovered slice satisfies:
///    ```text
///    normalize_confusables(original_slice) == normalized_text[norm_start..norm_end]
///    ```
///
/// This works because:
///
/// - For **confusable characters**, all replacement bytes map back to the
///   start of the original character.  The *next* entry after the replacement
///   maps to the first byte past the original character, so the `[start..end]`
///   range captures the full original character.
/// - For **non-confusable characters** (including multibyte), each byte maps
///   to its own original position, preserving a 1:1 byte correspondence.
/// - The **terminal sentinel** ensures `offset_map[normalized_text.len()]`
///   is always valid, covering matches that extend to end-of-string.
///
/// # Invariants
///
/// - `offset_map[0] == 0`
/// - `offset_map[normalized_text.len()] == s.len()`
/// - The mapping is monotonically non-decreasing.
/// - For any valid normalized byte range `[a..b]`:
///   `normalize_confusables(&s[offset_map[a]..offset_map[b]]) == normalized_text[a..b]`
pub fn build_offset_map(s: &str) -> (String, Vec<usize>) {
    // Pre-allocate conservatively.  In the worst case the normalized string is
    // longer (em-dash 3 bytes → "--" 2 bytes: actually shorter; ellipsis 3
    // bytes → "..." 3 bytes: same).  In practice normalized_len ≈ original_len.
    let mut normalized = String::with_capacity(s.len());
    // +1 for the terminal sentinel.
    let mut offset_map: Vec<usize> = Vec::with_capacity(s.len() + 1);

    for (orig_byte_offset, c) in s.char_indices() {
        match lookup(c) {
            Some(replacement) => {
                // Map each byte of the replacement string back to the start of
                // the original character.
                for _ in 0..replacement.len() {
                    offset_map.push(orig_byte_offset);
                }
                normalized.push_str(replacement);
            }
            None => {
                // Map each byte of the original character to its own position.
                let char_len = c.len_utf8();
                for i in 0..char_len {
                    offset_map.push(orig_byte_offset + i);
                }
                normalized.push(c);
            }
        }
    }

    // Terminal sentinel: one past the last byte of the normalized string maps
    // to one past the last byte of the original string.
    offset_map.push(s.len());

    debug_assert_eq!(offset_map.len(), normalized.len() + 1);
    debug_assert_eq!(*offset_map.last().unwrap(), s.len());

    (normalized, offset_map)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CONFUSABLE_MAP coverage ─────────────────────────────────────────

    #[test]
    fn normalize_left_double_quote() {
        assert_eq!(normalize_confusables("\u{201C}hello"), "\"hello");
    }

    #[test]
    fn normalize_right_double_quote() {
        assert_eq!(normalize_confusables("hello\u{201D}"), "hello\"");
    }

    #[test]
    fn normalize_left_single_quote() {
        assert_eq!(normalize_confusables("\u{2018}hi"), "'hi");
    }

    #[test]
    fn normalize_right_single_quote() {
        assert_eq!(normalize_confusables("hi\u{2019}"), "hi'");
    }

    #[test]
    fn normalize_em_dash() {
        assert_eq!(normalize_confusables("foo\u{2014}bar"), "foo--bar");
    }

    #[test]
    fn normalize_en_dash() {
        assert_eq!(normalize_confusables("10\u{2013}20"), "10-20");
    }

    #[test]
    fn normalize_ellipsis() {
        assert_eq!(normalize_confusables("wait\u{2026}"), "wait...");
    }

    #[test]
    fn normalize_nbsp() {
        assert_eq!(normalize_confusables("hello\u{00A0}world"), "hello world");
    }

    // ── Identity / passthrough ──────────────────────────────────────────

    #[test]
    fn normalize_pure_ascii_is_identity() {
        let ascii = "The quick brown fox jumps over the lazy dog. 0123456789 !@#$%^&*()";
        assert_eq!(normalize_confusables(ascii), ascii);
    }

    #[test]
    fn normalize_empty_string() {
        assert_eq!(normalize_confusables(""), "");
    }

    #[test]
    fn normalize_preserves_non_confusable_unicode() {
        // Emoji and CJK should pass through untouched.
        let s = "hello 🌍 世界";
        assert_eq!(normalize_confusables(s), s);
    }

    // ── has_confusables ─────────────────────────────────────────────────

    #[test]
    fn has_confusables_false_for_ascii() {
        assert!(!has_confusables("plain ASCII text"));
    }

    #[test]
    fn has_confusables_false_for_non_confusable_unicode() {
        assert!(!has_confusables("emoji 🎉 and 日本語"));
    }

    #[test]
    fn has_confusables_true_for_smart_quotes() {
        assert!(has_confusables("He said \u{201C}hello\u{201D}"));
    }

    #[test]
    fn has_confusables_true_for_nbsp() {
        assert!(has_confusables("a\u{00A0}b"));
    }

    #[test]
    fn has_confusables_true_for_em_dash() {
        assert!(has_confusables("a\u{2014}b"));
    }

    // ── detect_confusables ──────────────────────────────────────────────

    #[test]
    fn detect_returns_empty_for_ascii() {
        assert!(detect_confusables("plain text").is_empty());
    }

    #[test]
    fn detect_single_smart_quote() {
        let hits = detect_confusables("say \u{201C}hi\u{201D}");
        assert_eq!(hits.len(), 2);

        assert_eq!(hits[0].byte_offset, 4); // "say " is 4 bytes
        assert_eq!(hits[0].unicode_char, '\u{201C}');
        assert_eq!(hits[0].ascii_replacement, "\"");
        assert_eq!(hits[0].line_number, 1);

        // '\u{201C}' is 3 bytes, "hi" is 2 bytes → offset = 4+3+2 = 9
        assert_eq!(hits[1].byte_offset, 9);
        assert_eq!(hits[1].unicode_char, '\u{201D}');
        assert_eq!(hits[1].ascii_replacement, "\"");
        assert_eq!(hits[1].line_number, 1);
    }

    #[test]
    fn detect_confusables_tracks_line_numbers() {
        let s = "line one\nline\u{00A0}two\nline \u{201C}three\u{201D}\n";
        let hits = detect_confusables(s);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].line_number, 2); // NBSP on line 2
        assert_eq!(hits[1].line_number, 3); // left quote on line 3
        assert_eq!(hits[2].line_number, 3); // right quote on line 3
    }

    #[test]
    fn detect_consecutive_confusables() {
        // Two em-dashes in a row
        let s = "\u{2014}\u{2014}";
        let hits = detect_confusables(s);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].byte_offset, 0);
        assert_eq!(hits[1].byte_offset, 3); // em-dash is 3 bytes
    }

    #[test]
    fn detect_confusable_at_start_and_end() {
        let s = "\u{2018}hello\u{2019}";
        let hits = detect_confusables(s);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].byte_offset, 0);
        assert_eq!(hits[0].unicode_char, '\u{2018}');
        // '\u{2018}' is 3 bytes, "hello" is 5 bytes → offset = 8
        assert_eq!(hits[1].byte_offset, 8);
        assert_eq!(hits[1].unicode_char, '\u{2019}');
    }

    // ── build_offset_map ────────────────────────────────────────────────

    #[test]
    fn offset_map_pure_ascii() {
        let s = "abc";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "abc");
        // Each ASCII byte maps to itself, plus terminal sentinel.
        assert_eq!(map, vec![0, 1, 2, 3]);
    }

    #[test]
    fn offset_map_empty_string() {
        let (normalized, map) = build_offset_map("");
        assert_eq!(normalized, "");
        // Only the terminal sentinel.
        assert_eq!(map, vec![0]);
    }

    #[test]
    fn offset_map_terminal_sentinel() {
        let s = "hello";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "hello");
        assert_eq!(map.len(), normalized.len() + 1);
        assert_eq!(*map.last().unwrap(), s.len());
    }

    #[test]
    fn offset_map_smart_quotes() {
        // "\u{201C}hi\u{201D}" → "\"hi\""
        // Original bytes:  0..3 = '\u{201C}' (3 bytes), 3..5 = "hi", 5..8 = '\u{201D}' (3 bytes)
        // Normalized bytes: 0 = '"', 1..3 = "hi", 3 = '"'
        let s = "\u{201C}hi\u{201D}";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "\"hi\"");
        assert_eq!(normalized.len(), 4);
        assert_eq!(map.len(), 5); // 4 bytes + sentinel

        // Byte 0 of normalized ('"') maps to byte 0 of original ('\u{201C}' start)
        assert_eq!(map[0], 0);
        // Byte 1 of normalized ('h') maps to byte 3 of original
        assert_eq!(map[1], 3);
        // Byte 2 of normalized ('i') maps to byte 4 of original
        assert_eq!(map[2], 4);
        // Byte 3 of normalized ('"') maps to byte 5 of original ('\u{201D}' start)
        assert_eq!(map[3], 5);
        // Terminal sentinel
        assert_eq!(map[4], 8);
    }

    #[test]
    fn offset_map_em_dash() {
        // "a\u{2014}b" → "a--b"
        // Original: 0='a', 1..4='\u{2014}' (3 bytes), 4='b'
        // Normalized: 0='a', 1..3="--", 3='b'
        let s = "a\u{2014}b";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "a--b");
        assert_eq!(map.len(), 5); // 4 bytes + sentinel

        assert_eq!(map[0], 0); // 'a' → 'a'
        assert_eq!(map[1], 1); // first '-' → start of em-dash
        assert_eq!(map[2], 1); // second '-' → start of em-dash
        assert_eq!(map[3], 4); // 'b' → 'b'
        assert_eq!(map[4], 5); // sentinel
    }

    #[test]
    fn offset_map_ellipsis() {
        // "\u{2026}" → "..."
        // Original: 0..3 = '\u{2026}' (3 bytes)
        // Normalized: 0..3 = "..."  (3 bytes)
        let s = "\u{2026}";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "...");
        assert_eq!(map.len(), 4); // 3 bytes + sentinel

        // All three dots map to the start of the original ellipsis character.
        assert_eq!(map[0], 0);
        assert_eq!(map[1], 0);
        assert_eq!(map[2], 0);
        assert_eq!(map[3], 3); // sentinel
    }

    #[test]
    fn offset_map_nbsp() {
        // "a\u{00A0}b" → "a b"
        // Original: 0='a', 1..3='\u{00A0}' (2 bytes in UTF-8), 3='b'
        // Normalized: 0='a', 1=' ', 2='b'
        let s = "a\u{00A0}b";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "a b");
        assert_eq!(map.len(), 4); // 3 bytes + sentinel

        assert_eq!(map[0], 0); // 'a'
        assert_eq!(map[1], 1); // ' ' maps to NBSP start
        assert_eq!(map[2], 3); // 'b'
        assert_eq!(map[3], 4); // sentinel
    }

    #[test]
    fn offset_map_non_confusable_multibyte() {
        // Emoji passes through with per-byte identity mapping.
        // '🌍' is U+1F30D, 4 bytes in UTF-8.
        let s = "a🌍b";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, s); // no confusables → identical
        // 'a'=1 byte, '🌍'=4 bytes, 'b'=1 byte → 6 bytes + sentinel
        assert_eq!(map.len(), 7);
        assert_eq!(map[0], 0); // 'a'
        assert_eq!(map[1], 1); // '🌍' byte 0
        assert_eq!(map[2], 2); // '🌍' byte 1
        assert_eq!(map[3], 3); // '🌍' byte 2
        assert_eq!(map[4], 4); // '🌍' byte 3
        assert_eq!(map[5], 5); // 'b'
        assert_eq!(map[6], 6); // sentinel
    }

    #[test]
    fn offset_map_mixed_confusables_and_ascii() {
        // "He said \u{201C}yes\u{201D} \u{2014} no\u{2026}"
        let s = "He said \u{201C}yes\u{201D} \u{2014} no\u{2026}";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "He said \"yes\" -- no...");

        // Verify invariants.
        assert_eq!(map.len(), normalized.len() + 1);
        assert_eq!(map[0], 0);
        assert_eq!(*map.last().unwrap(), s.len());

        // Monotonically non-decreasing.
        for window in map.windows(2) {
            assert!(
                window[0] <= window[1],
                "offset_map not monotonic: {} > {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn offset_map_en_dash_single_char_replacement() {
        // "\u{2013}" is 3 bytes in UTF-8, maps to "-" (1 byte).
        let s = "\u{2013}";
        let (normalized, map) = build_offset_map(s);
        assert_eq!(normalized, "-");
        assert_eq!(map.len(), 2); // 1 byte + sentinel
        assert_eq!(map[0], 0); // '-' maps to start of en-dash
        assert_eq!(map[1], 3); // sentinel = original len
    }

    // ── Compound / integration scenarios ────────────────────────────────

    #[test]
    fn normalize_multiple_confusables_one_line() {
        let s = "\u{201C}hello\u{201D}\u{2014}world\u{2026}";
        assert_eq!(normalize_confusables(s), "\"hello\"--world...");
    }

    #[test]
    fn detect_then_normalize_roundtrip() {
        let original = "She said \u{201C}go\u{201D}";
        let hits = detect_confusables(original);
        assert_eq!(hits.len(), 2);

        let normalized = normalize_confusables(original);
        assert_eq!(normalized, "She said \"go\"");
        assert!(!has_confusables(&normalized));
    }

    #[test]
    fn build_offset_map_agrees_with_normalize() {
        let s = "a\u{201C}b\u{2014}c\u{00A0}d";
        let (from_map, _) = build_offset_map(s);
        let from_normalize = normalize_confusables(s);
        assert_eq!(from_map, from_normalize);
    }

    // ── Consumer-contract tests ─────────────────────────────────────────
    //
    // These tests simulate the exact pattern that the normalized-fallback
    // matcher will use: find a substring in the normalized text,
    // remap the span back to the original via offset_map, and verify that
    // normalizing the extracted original slice produces the matched text.

    /// Helper: find `pattern` in `normalized`, remap to original via
    /// `offset_map`, and return the original slice.  Panics if not found.
    fn remap_first_match<'a>(
        original: &'a str,
        normalized: &str,
        offset_map: &[usize],
        pattern: &str,
    ) -> &'a str {
        let norm_start = normalized
            .find(pattern)
            .expect("pattern not found in normalized text");
        let norm_end = norm_start + pattern.len();
        let orig_start = offset_map[norm_start];
        let orig_end = offset_map[norm_end];
        &original[orig_start..orig_end]
    }

    #[test]
    fn remap_smart_quotes_roundtrip() {
        // File content with smart quotes; model searches for ASCII quotes.
        let original = "She said \u{201C}stream through\u{201D} clearly";
        let (normalized, map) = build_offset_map(original);
        let pattern = "\"stream through\"";

        let orig_slice = remap_first_match(original, &normalized, &map, pattern);

        // The original slice should contain the smart-quoted region.
        assert_eq!(orig_slice, "\u{201C}stream through\u{201D}");
        // Normalizing it back must equal the pattern we searched for.
        assert_eq!(normalize_confusables(orig_slice), pattern);
    }

    #[test]
    fn remap_mixed_confusables_roundtrip() {
        // A realistic line with em-dash, smart quotes, and NBSP.
        let original = "use \u{201C}flag\u{201D}\u{00A0}\u{2014}\u{00A0}see docs";
        let (normalized, map) = build_offset_map(original);

        // Model searches for the ASCII equivalent of the whole middle section.
        let pattern = "\"flag\" -- see";
        let orig_slice = remap_first_match(original, &normalized, &map, pattern);

        assert_eq!(
            orig_slice,
            "\u{201C}flag\u{201D}\u{00A0}\u{2014}\u{00A0}see"
        );
        assert_eq!(normalize_confusables(orig_slice), pattern);
    }

    #[test]
    fn remap_match_at_end_of_string() {
        // Match that extends to the very end of the string (exercises
        // the terminal sentinel).
        let original = "wait\u{2026}";
        let (normalized, map) = build_offset_map(original);
        let pattern = "wait...";

        let orig_slice = remap_first_match(original, &normalized, &map, pattern);

        assert_eq!(orig_slice, original);
        assert_eq!(normalize_confusables(orig_slice), pattern);
    }
}
