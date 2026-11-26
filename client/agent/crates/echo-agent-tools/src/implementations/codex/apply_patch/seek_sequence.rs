//! Fuzzy line-sequence matcher for the codex apply-patch engine.
//!
//! Ported verbatim from `codex-rs/apply-patch/src/seek_sequence.rs`.
//!
//! Attempts to find a sequence of `pattern` lines within `lines` beginning at
//! or after `start`.  Matches are attempted with decreasing strictness:
//!
//! 1. **Exact match**
//! 2. **rstrip** — ignore trailing whitespace
//! 3. **trim** — ignore leading and trailing whitespace
//! 4. **Unicode normalise** — normalise common Unicode punctuation to ASCII
//!    equivalents (typographic dashes → `-`, smart quotes → `'`/`"`, etc.)

/// Find `pattern` within `lines` starting at `start`.
///
/// When `eof` is `true`, the search begins at the end of the file (so that
/// patterns intended to match file endings are applied at the end), falling
/// back to searching from `start` if needed.
///
/// Returns the starting index of the match, or `None` if not found.
///
/// # Edge cases
///
/// - Empty `pattern` → returns `Some(start)` (no-op match).
/// - `pattern.len() > lines.len()` → returns `None`.
pub fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }

    // When the pattern is longer than the available input there is no
    // possible match.
    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    // ── Pass 1: exact match ──────────────────────────────────────────
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }

    // ── Pass 2: rstrip match ─────────────────────────────────────────
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim_end() != pat.trim_end() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    // ── Pass 3: trim both sides ──────────────────────────────────────
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim() != pat.trim() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    // ── Pass 4: Unicode normalise ────────────────────────────────────
    // Normalise common Unicode punctuation to ASCII equivalents so that
    // diffs authored with plain ASCII characters can still be applied to
    // source files that contain typographic dashes / quotes, etc.
    fn normalise(s: &str) -> String {
        s.trim()
            .chars()
            .map(|c| match c {
                // Various dash / hyphen code-points → ASCII '-'
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                | '\u{2212}' => '-',
                // Fancy single quotes → '\''
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                // Fancy double quotes → '"'
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                // Non-breaking space and other odd spaces → normal space
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
                | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
                | '\u{3000}' => ' ',
                other => other,
            })
            .collect::<String>()
    }

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if normalise(&lines[i + p_idx]) != normalise(pat) {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }

    None
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::seek_sequence;

    fn to_vec(strings: &[&str]) -> Vec<String> {
        strings.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn test_exact_match_finds_sequence() {
        let lines = to_vec(&["foo", "bar", "baz"]);
        let pattern = to_vec(&["bar", "baz"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
    }

    #[test]
    fn test_rstrip_match_ignores_trailing_whitespace() {
        let lines = to_vec(&["foo   ", "bar\t\t"]);
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_trim_match_ignores_leading_and_trailing_whitespace() {
        let lines = to_vec(&["    foo   ", "   bar\t"]);
        let pattern = to_vec(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_pattern_longer_than_input_returns_none() {
        let lines = to_vec(&["just one line"]);
        let pattern = to_vec(&["too", "many", "lines"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
    }

    #[test]
    fn test_empty_pattern_returns_start() {
        let lines = to_vec(&["foo", "bar"]);
        let pattern: Vec<String> = vec![];
        assert_eq!(seek_sequence(&lines, &pattern, 1, false), Some(1));
    }

    #[test]
    fn test_eof_flag_searches_from_end() {
        let lines = to_vec(&["a", "b", "c", "b", "c"]);
        let pattern = to_vec(&["b", "c"]);
        // Without eof, finds the first occurrence.
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
        // With eof, prefers the last occurrence.
        assert_eq!(seek_sequence(&lines, &pattern, 0, true), Some(3));
    }

    #[test]
    fn test_unicode_normalise_matches_typographic_dashes() {
        // Line contains EN DASH (\u{2013}).
        let lines = to_vec(&["hello \u{2013} world"]);
        // Pattern uses plain ASCII dash.
        let pattern = to_vec(&["hello - world"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }
}
