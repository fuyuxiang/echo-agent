use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const DEFAULT_FONT_SIZE: f64 = 16.0;
pub const DEFAULT_LINE_HEIGHT: f64 = 1.1;
pub const DEFAULT_WRAP_WIDTH: f64 = 200.0;
pub const DEFAULT_CHAR_WIDTH: f64 = 8.0;
pub const DEFAULT_TEXT_HEIGHT: f64 = 24.0;

/// A single unbreakable token is kept whole (its box widens to fit it, matching
/// mermaid's default `htmlLabels`) unless it is wider than this many wrap-widths.
/// ~5x keeps the worst-case whole-token box near one target-width frame, so the
/// downstream rasterizer's scale-to-`target_width_px` stays ~1x and text stays
/// legible; memory is bounded separately by the consuming crate's raster caps.
const SINGLE_TOKEN_WIDTH_CAP_FACTOR: f64 = 5.0;

/// Identifier-boundary characters preferred as break points when an over-cap
/// token must be split.
const TOKEN_BREAK_CHARS: [char; 4] = ['_', '-', '.', '/'];

/// Display width of `text` in narrow-character units (East Asian wide
/// characters count as two).
pub fn display_width_units(text: &str) -> f64 {
    UnicodeWidthStr::width(text) as f64
}

/// Mirrors mermaid.js splitText.ts splitLineToFitWidth behavior for non-markdown labels.
/// Source: packages/mermaid/src/rendering-util/splitText.ts.
pub fn wrap_text_lines(text: &str, max_width: f64, char_width: f64) -> Vec<Vec<String>> {
    if text.is_empty() {
        return Vec::new();
    }
    let max_width = if max_width.is_finite() {
        max_width
    } else {
        f64::INFINITY
    };

    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            lines.push(vec![String::new()]);
            continue;
        }
        let words = split_line_to_words(trimmed);
        let wrapped = split_line_to_fit_width(words, max_width, char_width);
        lines.extend(wrapped);
    }

    lines
}

/// Matches mermaid.js createText.ts line-width checks using display-width
/// estimation.
pub fn line_width(line: &str, char_width: f64) -> f64 {
    if line.is_empty() {
        return 0.0;
    }
    display_width_units(line) * char_width
}

pub fn measure_wrapped_lines_with_font_size(
    lines: &[Vec<String>],
    char_width: f64,
    font_size: f64,
) -> (f64, f64) {
    let max_width = lines
        .iter()
        .map(|line| line_width_words(line, char_width))
        .fold(0.0, f64::max);
    (
        max_width,
        wrapped_text_height_with_font_size(lines.len(), font_size),
    )
}

pub fn wrapped_text_height_with_font_size(line_count: usize, font_size: f64) -> f64 {
    if line_count == 0 {
        return 0.0;
    }
    let font_size = normalized_font_size(font_size);
    let text_height = DEFAULT_TEXT_HEIGHT * font_size / DEFAULT_FONT_SIZE;
    let line_spacing = font_size * DEFAULT_LINE_HEIGHT;
    text_height + (line_count.saturating_sub(1)) as f64 * line_spacing
}

pub fn scale_char_width(char_width: f64, font_size: f64) -> f64 {
    char_width * normalized_font_size(font_size) / DEFAULT_FONT_SIZE
}

fn normalized_font_size(font_size: f64) -> f64 {
    if font_size.is_finite() && font_size > 0.0 {
        font_size
    } else {
        DEFAULT_FONT_SIZE
    }
}
fn split_line_to_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in text.split_whitespace() {
        words.push(word.to_string());
    }
    if words.is_empty() {
        words.push(String::new());
    }
    words
}

fn split_line_to_fit_width(
    words: Vec<String>,
    max_width: f64,
    char_width: f64,
) -> Vec<Vec<String>> {
    let mut remaining = std::collections::VecDeque::from(words);
    let mut lines: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    loop {
        if remaining.is_empty() {
            if !current.is_empty() {
                lines.push(current);
            }
            break;
        }

        let next_word = remaining.pop_front().unwrap_or_default();

        let mut line_with_next = current.clone();
        line_with_next.push(next_word.clone());

        if check_fit(&line_with_next, max_width, char_width) {
            current = line_with_next;
            continue;
        }

        if !current.is_empty() {
            lines.push(current);
            current = Vec::new();
            remaining.push_front(next_word);
            continue;
        }

        if !next_word.is_empty() {
            // Keep an unbreakable token whole so its box can widen (see const doc).
            let cap = max_width * SINGLE_TOKEN_WIDTH_CAP_FACTOR;
            if line_width(&next_word, char_width) <= cap {
                lines.push(vec![next_word]);
            } else {
                let (first, rest) = split_token_at_cap(&next_word, cap, char_width);
                lines.push(vec![first]);
                if !rest.is_empty() {
                    remaining.push_front(rest);
                }
            }
        }
    }

    lines
}

fn check_fit(words: &[String], max_width: f64, char_width: f64) -> bool {
    line_width_words(words, char_width) <= max_width
}

fn split_word_to_fit_width(word: &str, max_width: f64, char_width: f64) -> (String, String) {
    let graphemes: Vec<&str> = word.graphemes(true).collect();
    if graphemes.is_empty() {
        return (String::new(), String::new());
    }

    let mut used = Vec::new();
    let mut remaining_start = graphemes.len();
    for (idx, grapheme) in graphemes.iter().enumerate() {
        let mut candidate = used.clone();
        candidate.push(*grapheme);
        let candidate_str = candidate.concat();
        if line_width(&candidate_str, char_width) <= max_width || used.is_empty() {
            used = candidate;
            continue;
        }
        remaining_start = idx;
        break;
    }

    if used.is_empty() {
        used.push(graphemes[0]);
        remaining_start = 1;
    }

    let remaining = if remaining_start < graphemes.len() {
        graphemes[remaining_start..].concat()
    } else {
        String::new()
    };
    (used.concat(), remaining)
}

/// Splits an over-cap token: prefers the last identifier boundary (`_`, `-`,
/// `.`, `/`) within the cap-fitting prefix, otherwise falls back to the grapheme
/// break used elsewhere. Break points are identifier-char granular, so long
/// URLs/paths break at a separator instead of mid-segment.
fn split_token_at_cap(word: &str, cap: f64, char_width: f64) -> (String, String) {
    // Grapheme prefix that fits the cap; also guarantees forward progress, so it
    // is always a strict prefix here (the whole word is wider than the cap).
    let (graphemic_first, graphemic_rest) = split_word_to_fit_width(word, cap, char_width);
    // Break chars are single-byte ASCII, so the rfind byte index + 1 is a valid
    // char boundary that keeps the separator on the first line.
    if let Some(boundary) = graphemic_first.rfind(|c| TOKEN_BREAK_CHARS.contains(&c)) {
        let pos = boundary + 1;
        return (word[..pos].to_string(), word[pos..].to_string());
    }
    (graphemic_first, graphemic_rest)
}

pub fn line_width_words(words: &[String], char_width: f64) -> f64 {
    let joined = join_words(words);
    line_width(&joined, char_width)
}

fn join_words(words: &[String]) -> String {
    let mut out = String::new();
    for (idx, word) in words.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_long_single_token_whole_without_slicing() {
        // A single long identifier stays whole on one line (mermaid htmlLabels
        // behavior), instead of being hard-sliced mid-identifier.
        let label = "mark_filter_restore_context";
        let lines = wrap_text_lines(label, DEFAULT_WRAP_WIDTH, DEFAULT_CHAR_WIDTH);
        assert_eq!(lines, vec![vec![label.to_string()]]);
    }

    #[test]
    fn long_single_token_measures_wider_than_wrap_cap() {
        // Keeping the token whole means the measured text width is no longer
        // clamped to the wrap cap, so the node box widens to fit it.
        let lines = wrap_text_lines(
            "mark_filter_restore_context",
            DEFAULT_WRAP_WIDTH,
            DEFAULT_CHAR_WIDTH,
        );
        let (width, _height) =
            measure_wrapped_lines_with_font_size(&lines, DEFAULT_CHAR_WIDTH, DEFAULT_FONT_SIZE);
        assert!(
            width > DEFAULT_WRAP_WIDTH,
            "measured width {width} must exceed wrap cap {DEFAULT_WRAP_WIDTH}"
        );
    }

    #[test]
    fn long_token_with_trailing_words_keeps_token_on_first_line() {
        // The long leading token stays whole on its own line; the trailing
        // words wrap onto a following line instead of being merged into it.
        let lines = wrap_text_lines(
            "_render_sidebar_for_active column mgmt",
            DEFAULT_WRAP_WIDTH,
            DEFAULT_CHAR_WIDTH,
        );
        assert_eq!(
            lines,
            vec![
                vec!["_render_sidebar_for_active".to_string()],
                vec!["column".to_string(), "mgmt".to_string()],
            ]
        );
    }

    #[test]
    fn multi_word_label_still_wraps_at_spaces() {
        // Regression guard: a normal multi-word label that exceeds the wrap
        // width still wraps at spaces, with every word kept intact.
        let phrase = "the quick brown fox jumps over the lazy dog";
        let lines = wrap_text_lines(phrase, DEFAULT_WRAP_WIDTH, DEFAULT_CHAR_WIDTH);
        assert!(lines.len() >= 2, "long phrase must wrap: {lines:?}");
        let flat: Vec<String> = lines.iter().flatten().cloned().collect();
        let words: Vec<String> = phrase.split(' ').map(str::to_string).collect();
        assert_eq!(flat, words);
    }

    #[test]
    fn pathologically_long_token_breaks_on_identifier_boundary() {
        // A token wider than the cap is force-broken, but the break lands on an
        // identifier boundary ('_'), not mid-segment, and loses no graphemes.
        let token = "segment_".repeat(25);
        let cap = SINGLE_TOKEN_WIDTH_CAP_FACTOR * DEFAULT_WRAP_WIDTH;
        assert!(line_width(&token, DEFAULT_CHAR_WIDTH) > cap);
        let lines = wrap_text_lines(&token, DEFAULT_WRAP_WIDTH, DEFAULT_CHAR_WIDTH);
        assert!(
            lines.len() >= 2,
            "over-cap token must be force-broken: {lines:?}"
        );
        assert_eq!(lines[0].len(), 1, "each broken piece is a single word");
        assert!(
            lines[0][0].ends_with('_'),
            "first break must land on an identifier boundary, got {:?}",
            lines[0][0]
        );
        let rejoined: String = lines.iter().flatten().cloned().collect();
        assert_eq!(rejoined, token);
    }

    #[test]
    fn over_cap_token_without_break_char_falls_back_to_grapheme_break() {
        // No identifier boundary: the grapheme-break fallback still bounds each
        // line to the cap and loses no graphemes.
        let token = "a".repeat(200);
        let cap = SINGLE_TOKEN_WIDTH_CAP_FACTOR * DEFAULT_WRAP_WIDTH;
        assert!(line_width(&token, DEFAULT_CHAR_WIDTH) > cap);
        let lines = wrap_text_lines(&token, DEFAULT_WRAP_WIDTH, DEFAULT_CHAR_WIDTH);
        assert!(lines.len() >= 2, "over-cap token must be broken: {lines:?}");
        assert!(line_width(&lines[0].concat(), DEFAULT_CHAR_WIDTH) <= cap);
        let rejoined: String = lines.iter().flatten().cloned().collect();
        assert_eq!(rejoined, token);
    }

    #[test]
    fn over_cap_cjk_token_breaks_on_boundary_and_counts_wide_chars() {
        // Wide chars count as two narrow units; an over-cap CJK token with
        // separators still breaks at a `_`, never panics, and rejoins losslessly.
        assert_eq!(display_width_units("中"), 2.0);
        let token = "中文_".repeat(50);
        let cap = SINGLE_TOKEN_WIDTH_CAP_FACTOR * DEFAULT_WRAP_WIDTH;
        assert!(line_width(&token, DEFAULT_CHAR_WIDTH) > cap);
        let lines = wrap_text_lines(&token, DEFAULT_WRAP_WIDTH, DEFAULT_CHAR_WIDTH);
        assert!(lines.len() >= 2, "over-cap CJK token must break: {lines:?}");
        assert!(
            lines[0][0].ends_with('_'),
            "CJK break must land on a boundary, got {:?}",
            lines[0][0]
        );
        let rejoined: String = lines.iter().flatten().cloned().collect();
        assert_eq!(rejoined, token);
    }
}
