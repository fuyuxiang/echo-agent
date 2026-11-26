//! Line and string utility functions for ratatui text manipulation.

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub use super::tool_paths::{path_basename, path_for_tool_header, shorten_path};
pub use crate::util::byte_offset_at_width;

/// Clone a borrowed ratatui `Line` into an owned `'static` line.
pub fn line_to_static(line: &Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|s| Span {
                style: s.style,
                content: std::borrow::Cow::Owned(s.content.to_string()),
            })
            .collect(),
    }
}

/// Append owned copies of borrowed lines to `out`.
pub fn push_owned_lines(src: &[Line<'_>], out: &mut Vec<Line<'static>>) {
    for l in src {
        out.push(line_to_static(l));
    }
}

/// True for a character unsafe to render from untrusted/server text:
/// C0/C1 controls (the terminal-escape-injection vector) plus the Unicode
/// bidi-control and zero-width/format set (Trojan-Source spoofing) — U+061C,
/// U+200B–200F, U+202A–202E, U+2060–206F, U+FEFF.
///
/// Shared by every untrusted-text strip/scrub site (chip labels, toast error
/// scrub, settings editor input) so the set never drifts between them.
pub fn is_unsafe_display_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
        )
}

/// Polyfill for nightly-only [`str::floor_char_boundary`].
/// Snaps a byte index down to the nearest char boundary.
pub fn floor_char_boundary(s: &str, index: usize) -> usize {
    let index = index.min(s.len());
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `String`-owning delegate of [`crate::util::truncate_to_width`].
pub fn truncate_str(s: &str, max_width: usize) -> String {
    crate::util::truncate_to_width(s, max_width).into_owned()
}

/// Truncate a styled `Line` (multiple spans) to fit within `max_width` display columns.
///
/// Walks spans left-to-right, consuming width budget. When the budget is
/// exhausted mid-span, that span is truncated and `…` is appended. Spans
/// beyond the budget are dropped. All styles are preserved.
///
/// Returns the line unchanged if it already fits.
pub fn truncate_line(line: Line<'static>, max_width: usize) -> Line<'static> {
    if max_width == 0 {
        return Line::from(vec![]);
    }

    let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total <= max_width {
        return line;
    }

    // Need room for the ellipsis (1 column).
    let budget = max_width.saturating_sub(1);
    let mut used = 0usize;
    let mut out: Vec<Span<'static>> = Vec::new();

    for span in line.spans {
        let sw = span.content.width();
        if used + sw <= budget {
            // Entire span fits.
            used += sw;
            out.push(span);
        } else {
            // Partial fit — truncate this span.
            let remaining = budget - used;
            if remaining > 0 {
                let truncated = take_width(&span.content, remaining);
                out.push(Span::styled(truncated, span.style));
            }
            // Append ellipsis with the same style as the last span.
            let ellipsis_style = out.last().map(|s| s.style).unwrap_or_default();
            out.push(Span::styled("\u{2026}", ellipsis_style));
            return Line::from(out);
        }
    }

    // Shouldn't reach here (total > max_width checked above), but be safe.
    Line::from(out)
}

/// Clip or pad a styled `Line` to exactly `width` display columns.
///
/// Wider lines are clipped on grapheme boundaries (a multi-`char` grapheme like
/// `⚠\u{FE0F}` is never split) with no ellipsis; narrower lines are padded with
/// trailing spaces. This keeps a rendered row "self-owning" — the app writes a
/// real cell in every column, so a terminal drawing a glyph wider than the app
/// measured cannot strand a stale cell past the row (the markdown-table ghost
/// glyph bug). Width uses [`UnicodeWidthStr`], matching the table layout.
///
/// `width` must be a bounded display width: the pad branch allocates
/// `width - total` spaces.
pub fn fit_line_to_width<'a>(line: Line<'a>, width: usize) -> Line<'a> {
    let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total == width {
        return line;
    }

    let Line {
        style,
        alignment,
        mut spans,
    } = line;

    if total < width {
        spans.push(Span::raw(" ".repeat(width - total)));
        return Line {
            style,
            alignment,
            spans,
        };
    }

    // Wider than width: clip on grapheme boundaries, no ellipsis.
    let mut out: Vec<Span<'a>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let sw = span.content.width();
        if used + sw <= width {
            used += sw;
            out.push(span);
            if used == width {
                break;
            }
            continue;
        }
        // This span straddles the boundary — take whole graphemes that fit.
        let remaining = width - used;
        let mut taken = String::new();
        let mut taken_width = 0usize;
        for g in span.content.graphemes(true) {
            let gw = g.width();
            if taken_width + gw > remaining {
                break;
            }
            taken_width += gw;
            taken.push_str(g);
        }
        if !taken.is_empty() {
            out.push(Span::styled(taken, span.style));
            used += taken_width;
        }
        // A straddling wide grapheme leaves a 1-column gap; pad it.
        if used < width {
            out.push(Span::raw(" ".repeat(width - used)));
        }
        break;
    }

    Line {
        style,
        alignment,
        spans: out,
    }
}

/// Take the first `n` display columns from a string.
fn take_width(s: &str, n: usize) -> String {
    s[..byte_offset_at_width(s, n)].to_string()
}

/// Cascade-truncate multiple text elements to fit within `avail` display columns.
///
/// Returns `(type, description, activity, meta)` truncated to fit.
/// Priority (highest first): type, activity, meta. Description is truncated
/// first. If overhead (type + activity + meta) >= avail, description is dropped
/// and the remaining elements are cascaded: meta is dropped first, then
/// activity is truncated, then type.
pub fn cascade_truncate(
    avail: usize,
    type_text: &str,
    description: &str,
    activity_text: &str,
    meta_text: &str,
) -> (String, String, String, String) {
    let overhead = type_text.width() + activity_text.width() + meta_text.width();
    if overhead <= avail {
        let desc_max = avail - overhead;
        (
            type_text.to_string(),
            truncate_str(description, desc_max),
            activity_text.to_string(),
            meta_text.to_string(),
        )
    } else {
        let mut budget = avail;
        let td = if type_text.width() <= budget {
            budget -= type_text.width();
            type_text.to_string()
        } else {
            let s = truncate_str(type_text, budget);
            budget = 0;
            s
        };
        let ad = if budget == 0 {
            String::new()
        } else if activity_text.width() <= budget {
            budget -= activity_text.width();
            activity_text.to_string()
        } else {
            let s = truncate_str(activity_text, budget);
            budget = 0;
            s
        };
        let md = if budget == 0 {
            String::new()
        } else if meta_text.width() <= budget {
            meta_text.to_string()
        } else {
            truncate_str(meta_text, budget)
        };
        (td, String::new(), ad, md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_unsafe_display_char_covers_controls_and_bidi_format() {
        // Safe: ordinary printable text (incl. legitimate RTL letters).
        for c in ['a', ' ', '/', '\u{00e9}', '\u{05d0}'] {
            assert!(!is_unsafe_display_char(c), "{c:?} must be safe");
        }
        // Unsafe: C0/C1 controls + the full bidi-control / zero-width set.
        for c in [
            '\u{1b}', '\n', '\t', '\u{061C}', '\u{200B}', '\u{200F}', '\u{202E}', '\u{2066}',
            '\u{2069}', '\u{206F}', '\u{FEFF}',
        ] {
            assert!(
                is_unsafe_display_char(c),
                "{:#06x} must be unsafe",
                c as u32
            );
        }
    }

    // ── truncate_line tests ─────────────────────────────────────────

    #[test]
    fn truncate_line_fits() {
        let line = Line::from(vec![Span::raw("Hello "), Span::raw("world")]);
        let result = truncate_line(line, 20);
        assert_eq!(result.spans.len(), 2);
        assert_eq!(result.spans[0].content.as_ref(), "Hello ");
        assert_eq!(result.spans[1].content.as_ref(), "world");
    }

    #[test]
    fn truncate_line_cuts_mid_span() {
        let line = Line::from(vec![
            Span::raw("Edit "),
            Span::raw("very/long/path/to/file.rs"),
        ]);
        // Total = 29, budget = 15 → "Edit very/long…"
        let result = truncate_line(line, 15);
        let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with('\u{2026}'));
        assert!(text.width() <= 15);
    }

    #[test]
    fn truncate_line_drops_later_spans() {
        let line = Line::from(vec![
            Span::raw("Search "),
            Span::raw("pattern"),
            Span::raw(" in "),
            Span::raw("path"),
            Span::raw(" (5 matches)"),
        ]);
        let result = truncate_line(line, 18);
        let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with('\u{2026}'));
        assert!(text.width() <= 18);
    }

    #[test]
    fn truncate_line_zero_width() {
        let line = Line::from(vec![Span::raw("hello")]);
        let result = truncate_line(line, 0);
        assert!(result.spans.is_empty());
    }

    // ── fit_line_to_width tests ─────────────────────────────────────

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn fit_line_pads_short_line() {
        let line = Line::from(vec![Span::raw("│ a │")]);
        let out = fit_line_to_width(line, 10);
        assert_eq!(line_text(&out).width(), 10);
        assert_eq!(line_text(&out), "│ a │     ");
    }

    #[test]
    fn fit_line_exact_width_unchanged() {
        let line = Line::from(vec![Span::raw("hello")]);
        let out = fit_line_to_width(line, 5);
        assert_eq!(out.spans.len(), 1);
        assert_eq!(line_text(&out), "hello");
    }

    #[test]
    fn fit_line_clips_long_line_no_ellipsis() {
        let line = Line::from(vec![Span::raw("│ Column A │ Column B │")]);
        let out = fit_line_to_width(line, 8);
        assert_eq!(line_text(&out).width(), 8);
        assert_eq!(line_text(&out), "│ Column");
    }

    #[test]
    fn fit_line_does_not_split_emoji_grapheme() {
        // a(1)+b(1)+⚠️(2) = 4. Clipping to 3 must drop the width-2 grapheme
        // whole (never split it) and pad → "ab" + 1 space.
        let line = Line::from(vec![Span::raw("ab\u{26A0}\u{FE0F}")]);
        let out = fit_line_to_width(line, 3);
        assert_eq!(line_text(&out).width(), 3);
        assert_eq!(line_text(&out), "ab ");
    }

    #[test]
    fn fit_line_clips_grapheme_straddle_in_later_span() {
        // The straddle happens in a later span: keep "ab", then 1 col left →
        // ⚠️ (width 2) won't fit → dropped whole and padded.
        let line = Line::from(vec![Span::raw("ab"), Span::raw("\u{26A0}\u{FE0F}cd")]);
        let out = fit_line_to_width(line, 3);
        assert_eq!(line_text(&out).width(), 3);
        assert_eq!(line_text(&out), "ab ");
    }

    #[test]
    fn fit_line_drops_subsequent_spans_after_clip() {
        let line = Line::from(vec![
            Span::raw("hello"),
            Span::raw(" world"),
            Span::raw("!!!"),
        ]);
        let out = fit_line_to_width(line, 5);
        assert_eq!(line_text(&out), "hello");
        // The straddling/later spans must be dropped entirely.
        assert_eq!(out.spans.len(), 1);
    }

    #[test]
    fn fit_line_takes_partial_of_later_span() {
        let line = Line::from(vec![Span::raw("ab"), Span::raw("cdef")]);
        let out = fit_line_to_width(line, 4);
        assert_eq!(line_text(&out), "abcd");
        assert_eq!(line_text(&out).width(), 4);
    }

    #[test]
    fn fit_line_zero_width_returns_empty() {
        let line = Line::from(vec![Span::raw("│ a │")]);
        let out = fit_line_to_width(line, 0);
        assert_eq!(line_text(&out), "");
        assert_eq!(line_text(&out).width(), 0);
    }

    #[test]
    fn fit_line_preserves_span_styles_when_padding() {
        let bold = ratatui::style::Style::new().add_modifier(ratatui::style::Modifier::BOLD);
        let line = Line::from(vec![Span::styled("hi", bold)]);
        let out = fit_line_to_width(line, 5);
        assert_eq!(line_text(&out).width(), 5);
        assert!(
            out.spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn legacy_tool_path_api_remains_available_at_line_utils_path() {
        assert_eq!(shorten_path("verylongfilename.rs", 10), "verylongf…");
        assert_eq!(path_basename("/repo/src/main.rs", 80), "main.rs");
        assert_eq!(
            path_for_tool_header("/repo/src/main.rs", Some(80), "Read ".len()),
            "main.rs"
        );
        assert_eq!(
            path_for_tool_header("/repo/src/main.rs", None, "Read ".len()),
            "/repo/src/main.rs"
        );
    }

    // ── cascade_truncate tests ────────────────────────────────────

    #[test]
    fn cascade_truncate_all_fit() {
        let (t, d, a, m) =
            cascade_truncate(50, "type  ", "description", " \u{2014} running", "  meta");
        assert_eq!(t, "type  ");
        assert_eq!(d, "description");
        assert_eq!(a, " \u{2014} running");
        assert_eq!(m, "  meta");
    }

    #[test]
    fn cascade_truncate_desc_truncated() {
        let (t, d, a, m) = cascade_truncate(
            25,
            "type  ",
            "long description here",
            " \u{2014} running",
            "  meta",
        );
        assert_eq!(t, "type  ");
        assert_eq!(d, "lo\u{2026}");
        assert_eq!(a, " \u{2014} running");
        assert_eq!(m, "  meta");
    }

    #[test]
    fn cascade_truncate_desc_gone_meta_truncated() {
        // overhead = 6+10+6 = 22 > avail 20 → desc gone, type 6 + activity 10 + meta truncated to 4
        let (t, d, a, m) = cascade_truncate(20, "type  ", "desc", " \u{2014} running", "  meta");
        assert_eq!(t, "type  ");
        assert_eq!(d, "");
        assert_eq!(a, " \u{2014} running");
        assert_eq!(m, "  m\u{2026}");
    }

    #[test]
    fn cascade_truncate_meta_and_activity_gone() {
        // avail=8, type=6 fits (budget=2), activity truncated to 2, meta gone
        let (t, d, a, m) = cascade_truncate(8, "type  ", "desc", " \u{2014} running", "  meta");
        assert_eq!(t, "type  ");
        assert_eq!(d, "");
        assert_eq!(a, " \u{2026}");
        assert_eq!(m, "");
    }

    #[test]
    fn cascade_truncate_type_truncated() {
        let (t, d, a, m) = cascade_truncate(3, "type  ", "desc", " \u{2014} running", "  meta");
        assert_eq!(t, "ty\u{2026}");
        assert_eq!(d, "");
        assert_eq!(a, "");
        assert_eq!(m, "");
    }

    #[test]
    fn cascade_truncate_zero_avail() {
        let (t, d, a, m) = cascade_truncate(0, "type  ", "desc", " \u{2014} running", "  meta");
        assert_eq!(t, "");
        assert_eq!(d, "");
        assert_eq!(a, "");
        assert_eq!(m, "");
    }

    #[test]
    fn cascade_truncate_unicode() {
        // ✗ = 1 display column; — = 1 display column
        let (t, d, a, m) = cascade_truncate(10, "\u{2717}  ", "description", " \u{2014} run", "");
        assert_eq!(t, "\u{2717}  ");
        assert_eq!(d, "\u{2026}");
        assert_eq!(a, " \u{2014} run");
        assert_eq!(m, "");
    }

    #[test]
    fn cascade_truncate_overhead_equals_avail() {
        // overhead exactly equals avail → desc empty, everything else fits
        let (t, d, a, m) = cascade_truncate(22, "type  ", "desc", " \u{2014} running", "  meta");
        assert_eq!(t, "type  ");
        assert_eq!(d, "");
        assert_eq!(a, " \u{2014} running");
        assert_eq!(m, "  meta");
    }

    #[test]
    fn cascade_truncate_avail_one() {
        let (t, d, a, m) = cascade_truncate(1, "type", "desc", "act", "meta");
        assert_eq!(t, "\u{2026}");
        assert_eq!((d.as_str(), a.as_str(), m.as_str()), ("", "", ""));
    }

    #[test]
    fn cascade_truncate_all_empty() {
        let (t, d, a, m) = cascade_truncate(10, "", "", "", "");
        assert_eq!(
            (t.as_str(), d.as_str(), a.as_str(), m.as_str()),
            ("", "", "", "")
        );
    }
}
