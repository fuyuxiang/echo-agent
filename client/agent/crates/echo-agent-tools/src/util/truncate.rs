use std::borrow::Cow;

/// Default wrap width for soft-wrapping (used by bash, task_output).
pub const DEFAULT_SOFT_WRAP_WIDTH: usize = 2_000;

/// Default preview shown before a truncation footer.
pub const PREVIEW_SIZE: usize = 2_000;

/// Marker appended by `truncate_str_with_marker` when content is cut.
pub(crate) const TRUNCATION_MARKER: &str = "…";

/// Truncate a line to at most `max_chars` characters, respecting UTF-8 boundaries.
/// Content beyond `max_chars` is **discarded** and replaced with a marker.
///
/// Returns `Cow::Borrowed` if the line is already within the limit (zero-copy fast path).
/// Returns `Cow::Owned` with a truncation marker appended if the line was cut.
///
/// Use this for tools where content beyond the limit is genuinely not useful
/// to the model (e.g., grep match context) — clipped bytes are unrecoverable
/// by the caller. For tools where all content matters (bash, task_output),
/// use `soft_wrap_line` instead.
pub fn truncate_line(line: &str, max_chars: usize) -> Cow<'_, str> {
    // Fast path: if byte length ≤ max_chars, then char count ≤ max_chars
    // (every char is ≥1 byte). This avoids the O(n) chars().count() for
    // ASCII-only strings. For multi-byte UTF-8 this may false-negative
    // (byte_len > max_chars but char_count ≤ max_chars), falling through
    // to the slow path — that's a perf miss, not a correctness bug.
    if line.len() <= max_chars {
        return Cow::Borrowed(line);
    }
    let char_count = line.chars().count();
    if char_count <= max_chars {
        return Cow::Borrowed(line);
    }
    let end_byte = line
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    Cow::Owned(format!(
        "{} [... truncated ({} chars total)]",
        &line[..end_byte],
        char_count
    ))
}

/// Soft-wrap a long line by inserting newlines every `wrap_width` characters.
/// **All content is preserved** — nothing is discarded.
///
/// Returns `Cow::Borrowed` if the line is already within `wrap_width` (zero-copy).
///
/// This is the correct strategy for bash and task_output, where the total output
/// is already size-bounded (30KB) and the model benefits from seeing all of it.
/// The problem with long lines isn't size — it's that the model has no structure
/// to anchor on. Wrapping adds that structure without losing content.
pub fn soft_wrap_line(line: &str, wrap_width: usize) -> Cow<'_, str> {
    // Fast path: same byte-length optimization as truncate_line (see comment there).
    if line.len() <= wrap_width {
        return Cow::Borrowed(line);
    }
    let char_count = line.chars().count();
    if char_count <= wrap_width {
        return Cow::Borrowed(line);
    }
    let num_wraps = char_count.saturating_sub(1) / wrap_width;
    let mut result = String::with_capacity(line.len() + num_wraps);
    let mut chars_on_current_line = 0;
    for ch in line.chars() {
        if chars_on_current_line >= wrap_width {
            result.push('\n');
            chars_on_current_line = 0;
        }
        result.push(ch);
        chars_on_current_line += 1;
    }
    Cow::Owned(result)
}

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 boundary.
/// Returns the original string if it fits. No truncation marker is added.
///
/// Walks back from `max_bytes` until a char boundary is found. At most 3
/// steps back since UTF-8 multibyte sequences are at most 4 bytes.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Text on hand, and the size of the output it came from. The two differ when
/// the caller holds only part of a larger output and the reader still needs
/// the real size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartialOutput<'a> {
    text: &'a str,
    total_bytes: usize,
}

impl<'a> PartialOutput<'a> {
    pub fn whole(text: &'a str) -> Self {
        Self {
            text,
            total_bytes: text.len(),
        }
    }

    /// Part of an output of `total_bytes`.
    pub fn part_of(text: &'a str, total_bytes: usize) -> Self {
        Self {
            text,
            total_bytes: total_bytes.max(text.len()),
        }
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// Truncate output to a UTF-8-safe preview plus a model-visible footer.
///
/// The cap decides whether truncation happens. When triggered, the returned
/// value contains the first `preview_bytes` bytes snapped to a char boundary
/// followed by `[Output truncated - <N> bytes total...]`, where `N` is the
/// size of the whole output, not of the part on hand.
pub fn truncate_with_preview(
    output: PartialOutput<'_>,
    max_bytes: usize,
    preview_bytes: usize,
    footer_hint: Option<&str>,
) -> (String, bool) {
    let PartialOutput { text, total_bytes } = output;
    let whole = total_bytes <= text.len();
    if whole && text.len() <= max_bytes {
        return (text.to_string(), false);
    }

    let footer = match footer_hint {
        Some(hint) => format!("[Output truncated - {total_bytes} bytes total. {hint}]"),
        None => format!("[Output truncated - {total_bytes} bytes total]"),
    };
    // Text that fits the limit can still be part of a larger output; the
    // reader still needs the total size and where to find the rest.
    if text.len() <= max_bytes {
        return (format!("{text}\n\n{footer}"), true);
    }
    let preview = truncate_str(text, preview_bytes.min(text.len()));
    (format!("{preview}\n\n{footer}"), true)
}

/// Truncate a string to at most `max_bytes` bytes at a valid UTF-8 boundary,
/// appending `TRUNCATION_MARKER` when truncation actually happens.
///
/// Total byte length of the returned string is always `<= max_bytes`.
///
/// Returns `Cow::Borrowed` when the input already fits (no marker added --
/// only signal truncation when truncation actually happened). Returns
/// `Cow::Owned` with the marker appended when content was cut. When
/// `max_bytes == TRUNCATION_MARKER.len()`, returns just the marker so the
/// truncation signal is preserved. When `max_bytes < TRUNCATION_MARKER.len()`,
/// the marker cannot fit and we fall back to the marker-free `truncate_str`
/// behavior to honor the byte budget; this branch is only reachable when the
/// caller passes a pathologically tiny budget and is not exercised by any
/// production caller (`MIN_DESC_LENGTH` and other call-site minimums keep
/// the budget well above the marker size).
///
/// Use this when the reader needs to distinguish a natural string ending
/// from a truncation (e.g., model-visible listings). For purely visual
/// width-based truncation in the TUI, see `echo_agent_pager`'s own helpers.
pub fn truncate_str_with_marker(s: &str, max_bytes: usize) -> Cow<'_, str> {
    if s.len() <= max_bytes {
        return Cow::Borrowed(s);
    }
    if TRUNCATION_MARKER.len() > max_bytes {
        tracing::debug!(
            max_bytes,
            marker_len = TRUNCATION_MARKER.len(),
            "truncate_str_with_marker: budget too small for marker; truncation will be silent",
        );
        return Cow::Borrowed(truncate_str(s, max_bytes));
    }
    let mut end = max_bytes - TRUNCATION_MARKER.len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}{}", &s[..end], TRUNCATION_MARKER))
}

/// Find the largest byte index `<= index` that is a char boundary in `s`.
///
/// Polyfill for [`str::floor_char_boundary`] (stabilized in Rust 1.91; repo
/// toolchain is 1.90). Remove once the toolchain is bumped.
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Find the smallest byte index `>= index` that is a char boundary in `s`.
///
/// Polyfill for [`str::ceil_char_boundary`] (stabilized in Rust 1.91; repo
/// toolchain is 1.90). Remove once the toolchain is bumped.
pub fn ceil_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Estimate the number of tokens in a string using the bytes/4 heuristic.
/// Thin wrapper around [`echo_agent_token_estimation::estimate_tokens`] preserving
/// the historical `usize` return type used by tool-side callers
/// (`read_file`, `attach_file`, `inspect`, `compaction` file gates).
pub fn estimate_tokens(s: &str) -> usize {
    echo_agent_token_estimation::estimate_tokens(s) as usize
}

/// Estimate the number of chars per token using the bytes/4 heuristic.
/// Thin wrapper around [`echo_agent_token_estimation::estimate_chars`].
pub fn estimate_chars(s: u64) -> u64 {
    echo_agent_token_estimation::estimate_chars(s)
}

/// Human-readable size in powers of 1024: integral bytes (`512 B`), one
/// decimal above (`1.5 MB`). Every output fits nine columns.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB", "PB"];
    let mut val = bytes as f64 / 1024.0;
    for unit in UNITS {
        if val < 1023.95 {
            return format!("{val:.1} {unit}");
        }
        val /= 1024.0;
    }
    format!("{val:.1} EB")
}

/// Apply soft-wrapping to every line in a multi-line string.
/// All content is preserved. Lines already within `wrap_width` are untouched.
pub fn soft_wrap_lines(text: &str, wrap_width: usize) -> String {
    let mut result = String::with_capacity(text.len() + 256);
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        match soft_wrap_line(line, wrap_width) {
            Cow::Borrowed(s) => result.push_str(s),
            Cow::Owned(s) => result.push_str(&s),
        }
    }
    if text.ends_with('\n') && !text.is_empty() {
        result.push('\n');
    }
    result
}

/// Separator between the retained head and tail of a truncated output.
pub(crate) const FRONT_BACK_TRUNCATION_MARKER: &str = "\n\n... (output truncated) ...\n\n";

/// Truncate a string keeping the first half and last half of the character
/// budget, inserting a separator in the middle.
///
/// Returns `(result, was_truncated)`. When `s.len() <= max_chars` the
/// original string is returned unchanged and `was_truncated` is `false`.
pub fn truncate_front_and_back(s: &str, max_chars: usize) -> (String, bool) {
    if s.len() <= max_chars {
        return (s.to_string(), false);
    }
    let half = max_chars / 2;
    let front_end = s
        .char_indices()
        .nth(half)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let back_start = {
        let total_chars = s.chars().count();
        if total_chars <= half {
            0
        } else {
            s.char_indices()
                .nth(total_chars - half)
                .map(|(i, _)| i)
                .unwrap_or(0)
        }
    };
    let ellipsis = FRONT_BACK_TRUNCATION_MARKER;
    let mut result = String::with_capacity(front_end + ellipsis.len() + (s.len() - back_start));
    result.push_str(&s[..front_end]);
    result.push_str(ellipsis);
    result.push_str(&s[back_start..]);
    (result, true)
}

/// Truncate a string by keeping the first and last halves of a **character**
/// budget, inserting `"..."` in the middle. Used in the image-description
/// pipeline.
///
/// When `s.chars().count() <= max_chars` the input is returned unchanged.
/// Otherwise the result contains `⌊max_chars/2⌋` chars from the start,
/// the literal `"..."`, then `⌊max_chars/2⌋` chars from the end.
pub fn truncate_middle(s: &str, max_chars: usize) -> String {
    const MARKER: &str = "...";
    const MARKER_LEN: usize = MARKER.len();

    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    // The marker counts against the budget so the total never exceeds
    // `max_chars`.  When the budget is too small even for the marker we
    // fall back to a plain head-truncation.
    let remaining = max_chars.saturating_sub(MARKER_LEN);
    let front_count = remaining / 2;
    let back_count = remaining - front_count;

    // Front: first `front_count` chars.
    let front_end = s
        .char_indices()
        .nth(front_count)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    // Back: last `back_count` chars.
    let back_start = if char_count <= back_count {
        0
    } else {
        s.char_indices()
            .nth(char_count - back_count)
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let mut result = String::with_capacity(front_end + MARKER_LEN + (s.len() - back_start));
    result.push_str(&s[..front_end]);
    result.push_str(MARKER);
    result.push_str(&s[back_start..]);
    result
}

/// Truncate a multi-line string at line boundaries to fit within a character
/// budget.
///
/// Returns `(result, was_truncated)`. When the content already fits, the
/// joined+trimmed content is returned unchanged.
pub fn truncate_lines_to_char_budget(content: &str, budget: usize) -> (String, bool) {
    let trimmed = content.trim();
    if trimmed.len() <= budget {
        return (trimmed.to_string(), false);
    }
    // Find a valid UTF-8 char boundary at or before `budget` to avoid
    // panicking on multi-byte characters.
    let byte_end = budget.min(trimmed.len());
    let safe_end = (0..=byte_end)
        .rev()
        .find(|&i| trimmed.is_char_boundary(i))
        .unwrap_or(0);
    let truncated = &trimmed[..safe_end];
    let last_nl = truncated.rfind('\n');
    match last_nl {
        Some(idx) => (trimmed[..idx].trim().to_string(), true),
        None => (
            "... [First line would be too large to fit within character budget] ...".to_string(),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_scales_units() {
        let cases = [
            (0, "0 B"),
            (512, "512 B"),
            (1024, "1.0 KB"),
            (1536, "1.5 KB"),
            (1_048_575, "1.0 MB"),
            (1 << 50, "1.0 PB"),
            (u64::MAX, "16.0 EB"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(format_bytes(bytes), expected, "bytes = {bytes}");
        }
    }

    // ---- estimate_tokens ----

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_four_bytes() {
        assert_eq!(estimate_tokens("abcd"), 1);
    }

    #[test]
    fn estimate_tokens_rounds_down() {
        assert_eq!(estimate_tokens("abc"), 0);
        assert_eq!(estimate_tokens("abcdefg"), 1);
    }

    #[test]
    fn estimate_tokens_large() {
        assert_eq!(estimate_tokens(&"x".repeat(20_000)), 5_000);
    }

    // ---- truncate_line ----

    #[test]
    fn truncate_short_line_borrowed() {
        let r = truncate_line("hello", 2_000);
        assert!(matches!(r, Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_exact_limit_not_truncated() {
        let line = "a".repeat(2_000);
        assert!(matches!(truncate_line(&line, 2_000), Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_over_limit() {
        let line = "a".repeat(3_000);
        let r = truncate_line(&line, 2_000);
        assert!(r.contains("[... truncated (3000 chars total)]"));
        assert_eq!(r.split(" [... truncated").next().unwrap().len(), 2_000);
    }

    #[test]
    fn truncate_utf8_safe() {
        let line = "😀".repeat(2_001);
        let r = truncate_line(&line, 2_000);
        assert_eq!(
            r.split(" [... truncated").next().unwrap().chars().count(),
            2_000
        );
    }

    #[test]
    fn truncate_multibyte_char_count_under() {
        let line = "é".repeat(1_999); // 2 bytes each, 1 char each
        assert!(matches!(truncate_line(&line, 2_000), Cow::Borrowed(_)));
    }

    // ---- soft_wrap_line ----

    #[test]
    fn wrap_short_line_borrowed() {
        assert!(matches!(soft_wrap_line("hello", 2_000), Cow::Borrowed(_)));
    }

    #[test]
    fn wrap_preserves_all_content() {
        let line = "a".repeat(5_000);
        let r = soft_wrap_line(&line, 2_000);
        assert!(!r.contains("truncated"));
        let unwrapped: String = r.chars().filter(|c| *c != '\n').collect();
        assert_eq!(unwrapped.len(), 5_000);
    }

    #[test]
    fn wrap_inserts_newlines_correctly() {
        let line = "a".repeat(5_000);
        let r = soft_wrap_line(&line, 2_000);
        let lines: Vec<&str> = r.split('\n').collect();
        assert_eq!(lines.len(), 3); // 2000 + 2000 + 1000
        assert_eq!(lines[0].len(), 2_000);
        assert_eq!(lines[1].len(), 2_000);
        assert_eq!(lines[2].len(), 1_000);
    }

    #[test]
    fn wrap_utf8_safe() {
        let line = "😀".repeat(3_000);
        let r = soft_wrap_line(&line, 2_000);
        let lines: Vec<&str> = r.split('\n').collect();
        assert_eq!(lines[0].chars().count(), 2_000);
        assert_eq!(lines[1].chars().count(), 1_000);
    }

    // ---- truncate_str ----

    #[test]
    fn truncate_str_returns_original_when_fits() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("", 0), "");
    }

    #[test]
    fn truncate_str_ascii_exact_boundary() {
        assert_eq!(truncate_str("hello world", 5), "hello");
    }

    #[test]
    fn truncate_str_does_not_split_cjk() {
        // "日" is 3 bytes (0xE6 0x97 0xA5). Truncating at 2 must give "".
        assert_eq!(truncate_str("日本語", 2), "");
        assert_eq!(truncate_str("日本語", 3), "日");
        assert_eq!(truncate_str("日本語", 5), "日");
        assert_eq!(truncate_str("日本語", 6), "日本");
    }

    #[test]
    fn truncate_str_does_not_split_emoji() {
        // 🚀 is 4 bytes. Truncating at 1,2,3 must give "".
        assert_eq!(truncate_str("🚀🦀", 3), "");
        assert_eq!(truncate_str("🚀🦀", 4), "🚀");
        assert_eq!(truncate_str("🚀🦀", 7), "🚀");
        assert_eq!(truncate_str("🚀🦀", 8), "🚀🦀");
    }

    #[test]
    fn truncate_str_zero_budget_gives_empty() {
        assert_eq!(truncate_str("hello", 0), "");
        assert_eq!(truncate_str("日本語", 0), "");
    }

    // ---- truncate_str_with_marker ----

    #[test]
    fn truncate_with_marker_exact_boundary_no_marker() {
        // len == max_bytes: still fits, no marker.
        let r = truncate_str_with_marker("hello", 5);
        assert!(matches!(r, Cow::Borrowed(_)));
        assert_eq!(r, "hello");
    }

    #[test]
    fn truncate_with_marker_appends_marker_when_cut() {
        let r = truncate_str_with_marker("hello world", 10);
        // 10 bytes total: 7 content + 3 marker bytes.
        assert_eq!(r, "hello w…");
        assert!(r.len() <= 10);
        assert!(matches!(r, Cow::Owned(_)));
    }

    #[test]
    fn truncate_with_marker_respects_utf8_boundary() {
        // "日" is 3 bytes. With max_bytes=10 and the 3-byte marker,
        // target byte index is 7, which falls mid-char; walk back to 6
        // -> "日本" + marker.
        let r = truncate_str_with_marker("日本語abc", 10);
        assert_eq!(r, "日本…");
        assert!(r.len() <= 10);
        // Result is valid UTF-8 (3 chars: 日, 本, …).
        assert_eq!(r.chars().count(), 3);
    }

    // ---- truncate_with_preview ----

    #[test]
    fn truncate_with_preview_short_output_unchanged() {
        let (result, truncated) = truncate_with_preview(PartialOutput::whole("hello"), 10, 5, None);
        assert_eq!(result, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_with_preview_caps_large_output() {
        let output = "x".repeat(5_000_000);
        let (result, truncated) =
            truncate_with_preview(PartialOutput::whole(&output), 4_000, 2_000, None);

        assert!(truncated);
        assert!(result.len() < 2_200, "result was {} bytes", result.len());
        assert!(result.starts_with(&"x".repeat(2_000)));
        assert!(result.contains("[Output truncated - 5000000 bytes total]"));
    }

    #[test]
    fn truncate_with_preview_utf8_boundary() {
        let output = "😀".repeat(1_500);
        let (result, truncated) =
            truncate_with_preview(PartialOutput::whole(&output), 4_000, 2_001, None);

        assert!(truncated);
        assert!(result.starts_with(&"😀".repeat(500)));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_with_preview_with_footer_hint() {
        let output = "x".repeat(10_000);
        let (result, truncated) = truncate_with_preview(
            PartialOutput::whole(&output),
            4_000,
            2_000,
            Some("Use read_file for full content"),
        );

        assert!(truncated);
        assert!(result.contains("Use read_file for full content"));
    }

    /// A partial copy always states the size of the output it came from,
    /// whether or not the text on hand needed cutting.
    #[test]
    fn a_partial_copy_always_states_the_real_size() {
        // The text fits the limit: kept whole, footer added.
        let (result, truncated) = truncate_with_preview(
            PartialOutput::part_of("held", 5_000_000),
            4_000,
            2_000,
            Some("Use read_file for full content"),
        );
        assert!(truncated);
        assert!(result.starts_with("held"), "{result}");
        assert!(result.contains("5000000 bytes total"), "{result}");
        assert!(
            result.contains("Use read_file for full content"),
            "{result}"
        );

        // The text is over the limit: cut, and the footer keeps the total.
        let held = "x".repeat(10_000);
        let (result, truncated) =
            truncate_with_preview(PartialOutput::part_of(&held, 5_000_000), 4_000, 2_000, None);
        assert!(truncated);
        assert!(result.contains("5000000 bytes total"), "{result}");
    }

    #[test]
    fn truncate_with_preview_without_footer_hint() {
        let output = "x".repeat(10_000);
        let (result, truncated) =
            truncate_with_preview(PartialOutput::whole(&output), 4_000, 2_000, None);

        assert!(truncated);
        assert!(result.contains("[Output truncated - 10000 bytes total]"));
        assert!(!result.contains("full content"));
    }

    // ---- soft_wrap_lines ----

    #[test]
    fn wrap_lines_mixed() {
        let text = format!("short\n{}\nanother", "x".repeat(5_000));
        let result = soft_wrap_lines(&text, 2_000);
        let lines: Vec<&str> = result.split('\n').collect();
        assert_eq!(lines[0], "short");
        assert_eq!(lines[1].len(), 2_000); // first chunk of wrapped line
        assert_eq!(lines[4], "another");
        // Total content preserved
        let unwrapped: String = result.chars().filter(|c| *c != '\n').collect();
        let original: String = text.chars().filter(|c| *c != '\n').collect();
        assert_eq!(unwrapped, original);
    }

    #[test]
    fn wrap_lines_preserves_trailing_newline() {
        assert_eq!(soft_wrap_lines("hello\n", 2_000), "hello\n");
        assert_eq!(soft_wrap_lines("hello", 2_000), "hello");
    }

    // ---- truncate_front_and_back ----

    #[test]
    fn front_and_back_short_string_not_truncated() {
        let (result, truncated) = truncate_front_and_back("hello world", 100);
        assert_eq!(result, "hello world");
        assert!(!truncated);
    }

    #[test]
    fn front_and_back_keeps_both_ends() {
        let s = "a".repeat(100);
        let (result, truncated) = truncate_front_and_back(&s, 20);
        assert!(truncated);
        assert!(result.starts_with("aaaaaaaaaa")); // first 10
        assert!(result.ends_with("aaaaaaaaaa")); // last 10
        assert!(result.contains("... (output truncated) ..."));
    }

    #[test]
    fn front_and_back_exact_boundary() {
        let s = "a".repeat(20);
        let (result, truncated) = truncate_front_and_back(&s, 20);
        assert_eq!(result, s);
        assert!(!truncated);
    }

    // ---- truncate_lines_to_char_budget ----

    #[test]
    fn lines_budget_short_content_not_truncated() {
        let (result, truncated) = truncate_lines_to_char_budget("line1\nline2\nline3", 100);
        assert_eq!(result, "line1\nline2\nline3");
        assert!(!truncated);
    }

    #[test]
    fn lines_budget_truncates_at_line_boundary() {
        let content = "short\nmedium line\nthis is a longer line\nand another";
        let (result, truncated) = truncate_lines_to_char_budget(content, 25);
        assert!(truncated);
        assert!(!result.contains("this is a longer"));
        // Should end at a complete line
        assert!(result.ends_with("medium line") || result.ends_with("short"));
    }

    #[test]
    fn lines_budget_single_huge_line() {
        let content = "a".repeat(1000);
        let (result, truncated) = truncate_lines_to_char_budget(&content, 50);
        assert!(truncated);
        assert!(result.contains("character budget"));
    }

    // ---- floor_char_boundary / ceil_char_boundary ----

    #[test]
    fn floor_boundary_ascii() {
        assert_eq!(floor_char_boundary("hello", 3), 3);
    }

    #[test]
    fn floor_boundary_mid_cjk() {
        // "日" = 3 bytes. Index 1 or 2 should snap back to 0.
        assert_eq!(floor_char_boundary("日本", 1), 0);
        assert_eq!(floor_char_boundary("日本", 2), 0);
        assert_eq!(floor_char_boundary("日本", 3), 3);
    }

    #[test]
    fn floor_boundary_past_end() {
        assert_eq!(floor_char_boundary("hi", 100), 2);
    }

    #[test]
    fn ceil_boundary_ascii() {
        assert_eq!(ceil_char_boundary("hello", 3), 3);
    }

    #[test]
    fn ceil_boundary_mid_cjk() {
        // "日" = 3 bytes. Index 1 or 2 should snap forward to 3.
        assert_eq!(ceil_char_boundary("日本", 1), 3);
        assert_eq!(ceil_char_boundary("日本", 2), 3);
        assert_eq!(ceil_char_boundary("日本", 3), 3);
    }

    #[test]
    fn ceil_boundary_past_end() {
        assert_eq!(ceil_char_boundary("hi", 100), 2);
    }

    // ---- truncate_middle ----

    #[test]
    fn truncate_middle_short_string_unchanged() {
        assert_eq!(truncate_middle("hello", 10), "hello");
    }

    #[test]
    fn truncate_middle_exact_limit_unchanged() {
        let s = "a".repeat(20);
        assert_eq!(truncate_middle(&s, 20), s);
    }

    #[test]
    fn truncate_middle_keeps_both_ends() {
        // 26 chars, budget 10 → remaining=7, front=3, back=4
        let s = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_middle(s, 10);
        assert!(result.starts_with("abc"));
        assert!(result.ends_with("wxyz"));
        assert!(result.contains("..."));
        // Total must not exceed budget.
        assert!(
            result.chars().count() <= 10,
            "result exceeds budget: {} chars: {result}",
            result.chars().count()
        );
    }

    #[test]
    fn truncate_middle_respects_budget() {
        // Example: 50_000 chars at 12_000 limit.
        let s = "a".repeat(50_000);
        let result = truncate_middle(&s, 12_000);
        assert_eq!(
            result.chars().count(),
            12_000,
            "result should be exactly the budget"
        );
        assert!(result.contains("..."));
    }

    #[test]
    fn truncate_middle_utf8_safe() {
        let s = "😀".repeat(100);
        let result = truncate_middle(&s, 10);
        // remaining=7, front=3, back=4 → 3 emoji + "..." + 4 emoji = 10
        assert_eq!(result.chars().count(), 10);
        assert!(result.contains("..."));
    }
}
