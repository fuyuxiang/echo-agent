//! Optional Unicode Bidirectional Algorithm (UAX #9) for LTR terminal painting.
//!
//! **Off by default.** Many terminals already run implicit bidi; reordering in
//! the app double-flips text on those hosts. Enable only when the terminal does
//! not (`[scrollback.display] rtl_bidi = true`).
//!
//! When enabled (via [`set_line_safe_bidi`](super::SafeBuf::set_line_safe_bidi)
//! on scrollback/list content): strip bidi override/isolate controls, reorder
//! full display rows, reverse RTL runs by grapheme cluster, and mirror paired
//! punctuation. Table rows stay logical so columns align. Base direction is
//! resolved per painted row — a soft-wrapped continuation that starts with
//! Latin may differ from the paragraph's first row.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_bidi::Level;
use unicode_bidi::{BidiClass, BidiInfo, bidi_class};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

static RTL_BIDI_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether optional app-side bidi reordering is enabled (default false).
#[inline]
pub fn is_enabled() -> bool {
    RTL_BIDI_ENABLED.load(Ordering::Relaxed)
}

/// Set the process-wide enable latch (called when appearance config loads).
pub fn set_enabled(enabled: bool) {
    RTL_BIDI_ENABLED.store(enabled, Ordering::Relaxed);
}

/// True when `text` contains strong RTL or Arabic-number characters.
#[inline]
pub fn needs_bidi(text: &str) -> bool {
    text.chars().any(is_rtl_affecting)
}

#[inline]
fn is_rtl_affecting(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::AN)
}

/// Bidi override / isolate / embedding controls only (not ZWJ/ZWNJ joiners).
#[inline]
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn strip_bidi_controls(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_bidi_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().filter(|c| !is_bidi_control(*c)).collect())
}

/// Paragraph base level for a logical string (auto per UAX #9 P2/P3).
pub(crate) fn paragraph_level(text: &str) -> Level {
    let cleaned = strip_bidi_controls(text);
    let bidi = BidiInfo::new(cleaned.as_ref(), None);
    bidi.paragraphs
        .first()
        .map(|p| p.level)
        .unwrap_or_else(Level::ltr)
}

/// Logical → visual when enabled and needed; otherwise borrows.
/// When enabled, always strips bidi controls even for pure LTR.
pub fn visual_text(text: &str) -> Cow<'_, str> {
    if !is_enabled() {
        return Cow::Borrowed(text);
    }
    let cleaned = strip_bidi_controls(text);
    if is_table_row(cleaned.as_ref()) || !needs_bidi(cleaned.as_ref()) {
        return cleaned;
    }
    let level = paragraph_level(cleaned.as_ref());
    Cow::Owned(visual_text_with_level(cleaned.as_ref(), level))
}

/// Reorder with an explicit paragraph base (for tests / future wrap wiring).
pub(crate) fn visual_text_with_level(text: &str, level: Level) -> String {
    if text.contains('\n') {
        return text
            .split('\n')
            .map(|line| {
                if needs_bidi(line) && !is_table_row(line) {
                    visual_text_line(line, level)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    visual_text_line(text, level)
}

fn visual_text_line(text: &str, level: Level) -> String {
    debug_assert!(!text.contains('\n'));
    if is_table_row(text) {
        return text.to_string();
    }
    let cleaned = strip_bidi_controls(text);
    let prefix = chrome_prefix_len(cleaned.as_ref());
    if prefix >= cleaned.len() {
        return cleaned.into_owned();
    }
    let (chrome, body) = cleaned.split_at(prefix);
    if !needs_bidi(body) {
        return cleaned.into_owned();
    }
    let mut out = String::with_capacity(cleaned.len());
    out.push_str(chrome);
    out.push_str(&reorder_body(body, level));
    out
}

/// Logical → visual styled line. `None` when disabled or no change needed.
pub fn visual_line(line: &Line<'_>) -> Option<Line<'static>> {
    if !is_enabled() {
        return None;
    }
    visual_line_with_level(line, None)
}

/// Reorder a line using `level` when provided (shared wrap base).
pub(crate) fn visual_line_with_level(
    line: &Line<'_>,
    level: Option<Level>,
) -> Option<Line<'static>> {
    if !is_enabled() {
        return None;
    }

    let has_rtl = line.spans.iter().any(|s| needs_bidi(s.content.as_ref()));
    let has_controls = line
        .spans
        .iter()
        .any(|s| s.content.chars().any(is_bidi_control));
    if !has_rtl && !has_controls {
        return None;
    }

    // Flatten while stripping controls so style bounds match cleaned bytes.
    let mut flat = String::new();
    let mut span_bounds: Vec<(Range<usize>, Style)> = Vec::with_capacity(line.spans.len());
    for span in &line.spans {
        let start = flat.len();
        for c in span.content.chars() {
            if !is_bidi_control(c) {
                flat.push(c);
            }
        }
        if flat.len() > start {
            span_bounds.push((start..flat.len(), span.style));
        }
    }
    if flat.is_empty() || is_table_row(&flat) {
        return None;
    }

    let para_level = level.unwrap_or_else(|| paragraph_level(&flat));
    let prefix = chrome_prefix_len(&flat);
    let body = &flat[prefix..];

    // Controls-only LTR: rebuild the cleaned line without reordering.
    if body.is_empty() || !needs_bidi(body) {
        if !has_controls {
            return None;
        }
        let mut out_spans: Vec<Span<'static>> = Vec::new();
        append_graphemes_styled(&flat, 0, &span_bounds, &mut out_spans, false);
        let mut visual = Line::from(out_spans);
        visual.style = line.style;
        visual.alignment = line.alignment;
        return Some(visual);
    }

    let mut out_spans: Vec<Span<'static>> = Vec::new();
    if prefix > 0 {
        append_graphemes_styled(&flat[..prefix], 0, &span_bounds, &mut out_spans, false);
    }
    append_reordered_body(body, prefix, &span_bounds, &mut out_spans, para_level);

    let mut visual = Line::from(out_spans);
    visual.style = line.style;
    visual.alignment = line.alignment;
    Some(visual)
}

/// Logical characters covering visual columns `[vis_start, vis_end)`, in logical order.
pub fn logical_slice_for_visual_cols(text: &str, vis_start: usize, vis_end: usize) -> String {
    if vis_start >= vis_end || text.is_empty() {
        return String::new();
    }
    if !is_enabled() {
        return slice_display_cols(text, vis_start, vis_end);
    }
    // Classify on the stripped string so table/needs_bidi decisions match paint
    // (which strips first). A leading bidi control must not flip the decision.
    let cleaned = strip_bidi_controls(text);
    let text = cleaned
        .as_ref()
        .split('\n')
        .next()
        .unwrap_or(cleaned.as_ref());
    if !needs_bidi(text) || is_table_row(text) {
        return slice_display_cols(text, vis_start, vis_end);
    }
    let prefix = chrome_prefix_len(text);
    let prefix_cols = str_cells(&text[..prefix]);
    let mut out = String::new();

    if vis_start < prefix_cols {
        out.push_str(&slice_display_cols(
            &text[..prefix],
            vis_start,
            vis_end.min(prefix_cols),
        ));
    }
    if vis_end <= prefix_cols {
        return out;
    }

    let body = &text[prefix..];
    let body_vs = vis_start.saturating_sub(prefix_cols);
    let body_ve = vis_end.saturating_sub(prefix_cols);
    if body_vs >= body_ve {
        return out;
    }
    if !needs_bidi(body) {
        out.push_str(&slice_display_cols(body, body_vs, body_ve));
        return out;
    }

    let level = paragraph_level(text);
    let graphemes: Vec<&str> = body.graphemes(true).collect();
    let visual_order = visual_grapheme_order(body, level);
    let mut vis_col_of = vec![0usize; graphemes.len()];
    let mut vcol = 0usize;
    for &gi in &visual_order {
        vis_col_of[gi] = vcol;
        vcol += UnicodeWidthStr::width(graphemes[gi]);
    }
    for (gi, g) in graphemes.iter().enumerate() {
        let w = UnicodeWidthStr::width(*g);
        let vc = vis_col_of[gi];
        if w == 0 {
            continue;
        }
        if vc < body_ve && vc + w > body_vs {
            out.push_str(g);
        }
    }
    out
}

/// Inverse of the paint reorder: the logical display column of the grapheme
/// painted at `visual_col`. For surfaces that keep selection endpoints in
/// logical columns but hit-test painted cells (the block viewer drag). Identity
/// when reordering is off / a table row / no RTL; clamps past-end to the width.
pub fn visual_col_to_logical_col(text: &str, visual_col: usize) -> usize {
    if !is_enabled() {
        return visual_col;
    }
    let cleaned = strip_bidi_controls(text);
    let text = cleaned
        .as_ref()
        .split('\n')
        .next()
        .unwrap_or(cleaned.as_ref());
    if !needs_bidi(text) || is_table_row(text) {
        return visual_col;
    }
    let prefix = chrome_prefix_len(text);
    let prefix_cols = str_cells(&text[..prefix]);
    let body = &text[prefix..];
    // Chrome is painted logically (not reordered), so columns there are 1:1.
    if visual_col < prefix_cols || !needs_bidi(body) {
        return visual_col.min(prefix_cols + str_cells(body));
    }

    let body_vis = visual_col - prefix_cols;
    let level = paragraph_level(text);
    let graphemes: Vec<&str> = body.graphemes(true).collect();
    let order = visual_grapheme_order(body, level);
    let mut logical_col_of = vec![0usize; graphemes.len()];
    let mut lc = 0usize;
    for (gi, g) in graphemes.iter().enumerate() {
        logical_col_of[gi] = lc;
        lc += UnicodeWidthStr::width(*g);
    }
    let mut vcol = 0usize;
    for &gi in &order {
        let w = UnicodeWidthStr::width(graphemes[gi]);
        if w == 0 {
            continue;
        }
        if body_vis < vcol + w {
            return prefix_cols + logical_col_of[gi];
        }
        vcol += w;
    }
    prefix_cols + str_cells(body)
}

/// Map logical display columns to visual column ranges.
pub fn logical_cols_to_visual(
    text: &str,
    logical_start: usize,
    logical_end: usize,
) -> Vec<(usize, usize)> {
    if logical_start >= logical_end || text.is_empty() {
        return Vec::new();
    }
    if !is_enabled() {
        return vec![(logical_start, logical_end)];
    }
    let cleaned = strip_bidi_controls(text);
    let text = cleaned
        .as_ref()
        .split('\n')
        .next()
        .unwrap_or(cleaned.as_ref());
    if !needs_bidi(text) || is_table_row(text) {
        return vec![(logical_start, logical_end)];
    }
    let prefix = chrome_prefix_len(text);
    let prefix_cols = str_cells(&text[..prefix]);
    if logical_end <= prefix_cols {
        return vec![(logical_start, logical_end)];
    }

    let body = &text[prefix..];
    if !needs_bidi(body) {
        return vec![(logical_start, logical_end)];
    }

    let body_log_start = logical_start.saturating_sub(prefix_cols);
    let body_log_end = logical_end.saturating_sub(prefix_cols);
    let mut ranges = Vec::new();
    if logical_start < prefix_cols {
        ranges.push((logical_start, prefix_cols.min(logical_end)));
    }
    if body_log_start < body_log_end {
        let level = paragraph_level(text);
        for (vs, ve) in body_logical_cols_to_visual(body, body_log_start, body_log_end, level) {
            ranges.push((vs + prefix_cols, ve + prefix_cols));
        }
    }
    merge_adjacent_ranges(ranges)
}

fn is_table_row(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(ch)
            if ('\u{2500}'..='\u{257F}').contains(&ch) && ch != '\u{2502}' && ch != '\u{2503}' =>
        {
            true
        }
        Some('\u{2502}') => {
            let mut in_prefix = true;
            for ch in chars {
                if in_prefix && (ch == '\u{2502}' || ch == ' ') {
                    continue;
                }
                in_prefix = false;
                if ch == '\u{2502}' {
                    return true;
                }
            }
            false
        }
        Some('|') => true,
        _ => false,
    }
}

fn chrome_prefix_len(text: &str) -> usize {
    // Peel nested chrome tiers: a blockquote bar can be followed by a list
    // marker (`│ • …`, `│ 1. …`). Paint reorders the full line, so if only the
    // bar were peeled the marker would join the reordered body and move under
    // RTL — and the marker-only region map would then disagree with paint. Peel
    // the bar, then a single marker on the remainder, so both stay left-anchored
    // and the region map matches the painted body.
    let bq = blockquote_prefix_len(text);
    bq + marker_prefix_len(&text[bq..])
}

fn marker_prefix_len(text: &str) -> usize {
    // `\u{25C8} ` / `\u{2666} ` are the group-header diamond (see
    // `group_header_chrome_prefix`); keep it left-anchored like other markers.
    for prefix in [
        "$ ",
        "\u{276F} ",
        "> ",
        "\u{21BB}  ",
        "• ",
        "- ",
        "* ",
        "\u{25C8} ",
        "\u{2666} ",
    ] {
        if text.starts_with(prefix) {
            return prefix.len();
        }
    }
    // Ordered list markers: "1. ", "12. ", etc.
    {
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' ' {
            return i + 2;
        }
    }
    let spaces = text.bytes().take_while(|&b| b == b' ').count();
    if spaces > 0 && spaces < text.len() {
        spaces
    } else {
        0
    }
}

fn blockquote_prefix_len(text: &str) -> usize {
    const BAR_BYTES: usize = '\u{2502}'.len_utf8();
    let mut len = 0;
    let mut chars = text.chars();
    while let Some('\u{2502}') = chars.next() {
        if chars.next() == Some(' ') {
            len += BAR_BYTES + 1;
        } else {
            break;
        }
    }
    len
}

fn reorder_body(body: &str, level: Level) -> String {
    let bidi = BidiInfo::new(body, Some(level));
    let mut out = String::with_capacity(body.len());
    for para in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        for run in runs {
            let slice = &body[run.clone()];
            if levels[run.start].is_rtl() {
                for g in slice.graphemes(true).collect::<Vec<_>>().into_iter().rev() {
                    out.push_str(&mirror_grapheme(g));
                }
            } else {
                out.push_str(slice);
            }
        }
    }
    out
}

fn append_reordered_body(
    body: &str,
    body_byte_base: usize,
    span_bounds: &[(Range<usize>, Style)],
    out: &mut Vec<Span<'static>>,
    level: Level,
) {
    let bidi = BidiInfo::new(body, Some(level));
    for para in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        for run in runs {
            let slice = &body[run.clone()];
            let abs = body_byte_base + run.start;
            let rtl = levels[run.start].is_rtl();
            append_graphemes_styled(slice, abs, span_bounds, out, rtl);
        }
    }
}

fn append_graphemes_styled(
    slice: &str,
    abs_byte_start: usize,
    span_bounds: &[(Range<usize>, Style)],
    out: &mut Vec<Span<'static>>,
    reverse_rtl: bool,
) {
    let mut graphemes: Vec<(usize, &str)> = slice.grapheme_indices(true).collect();
    if reverse_rtl {
        graphemes.reverse();
    }
    for (rel, g) in graphemes {
        let mirrored;
        let text = if reverse_rtl {
            mirrored = mirror_grapheme(g);
            mirrored.as_str()
        } else {
            g
        };
        // `rel` is the original byte offset within the slice (grapheme_indices).
        append_str_styled(text, abs_byte_start + rel, span_bounds, out);
    }
}

fn append_str_styled(
    s: &str,
    abs_byte: usize,
    span_bounds: &[(Range<usize>, Style)],
    out: &mut Vec<Span<'static>>,
) {
    let style = style_at(abs_byte, span_bounds);
    if let Some(last) = out.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(s);
        return;
    }
    out.push(Span::styled(s.to_string(), style));
}

fn style_at(byte: usize, span_bounds: &[(Range<usize>, Style)]) -> Style {
    match span_bounds.binary_search_by(|(range, _)| {
        if byte < range.start {
            std::cmp::Ordering::Greater
        } else if byte >= range.end {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => span_bounds[i].1,
        Err(_) => Style::default(),
    }
}

fn mirror_grapheme(g: &str) -> String {
    g.chars()
        .map(|c| unicode_bidi_mirroring::get_mirrored(c).unwrap_or(c))
        .collect()
}

fn visual_grapheme_order(body: &str, level: Level) -> Vec<usize> {
    let grapheme_starts: Vec<usize> = body.grapheme_indices(true).map(|(i, _)| i).collect();
    let bidi = BidiInfo::new(body, Some(level));
    let mut order = Vec::with_capacity(grapheme_starts.len());
    for para in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        for run in runs {
            let mut idxs: Vec<usize> = grapheme_starts
                .iter()
                .enumerate()
                .filter(|(_, b)| **b >= run.start && **b < run.end)
                .map(|(i, _)| i)
                .collect();
            if levels[run.start].is_rtl() {
                idxs.reverse();
            }
            order.extend(idxs);
        }
    }
    order
}

fn body_logical_cols_to_visual(
    body: &str,
    logical_start: usize,
    logical_end: usize,
    level: Level,
) -> Vec<(usize, usize)> {
    let graphemes: Vec<&str> = body.graphemes(true).collect();
    let mut logical_meta: Vec<(usize, usize)> = Vec::new();
    let mut col = 0usize;
    for g in &graphemes {
        let w = UnicodeWidthStr::width(*g);
        logical_meta.push((col, w));
        col += w;
    }
    let order = visual_grapheme_order(body, level);
    let mut selected = Vec::new();
    let mut vcol = 0usize;
    for &gi in &order {
        let (lc, width) = logical_meta[gi];
        if width > 0 && lc < logical_end && lc + width > logical_start {
            selected.push((vcol, vcol + width));
        }
        vcol += width;
    }
    merge_adjacent_ranges(selected)
}

fn slice_display_cols(text: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    let mut out = String::new();
    let mut col = 0usize;
    for g in text.graphemes(true) {
        let w = UnicodeWidthStr::width(g);
        let next = col + w;
        if next > start && col < end {
            out.push_str(g);
        }
        col = next;
        if col >= end {
            break;
        }
    }
    out
}

/// Painted cell width: sum of per-grapheme widths, matching what the renderer
/// draws and the rest of the column math (a width-collapsed cluster such as a
/// ZWJ emoji occupies its cluster width, not the sum of its code points').
fn str_cells(s: &str) -> usize {
    s.graphemes(true).map(UnicodeWidthStr::width).sum()
}

fn merge_adjacent_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|(s, _)| *s);
    let mut merged = Vec::with_capacity(ranges.len());
    let (mut cs, mut ce) = ranges[0];
    for &(s, e) in &ranges[1..] {
        if s <= ce {
            ce = ce.max(e);
        } else {
            merged.push((cs, ce));
            cs = s;
            ce = e;
        }
    }
    merged.push((cs, ce));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::SafeBuf;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    const AR: &str = "سلام";
    const AR_V: &str = "مالس";
    const FA: &str = "خوب";
    const FA_V: &str = "بوخ";

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnabledGuard(bool);
    impl Drop for EnabledGuard {
        fn drop(&mut self) {
            set_enabled(self.0);
        }
    }

    fn with_enabled<R>(f: impl FnOnce() -> R) -> R {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Restore on scope exit even if `f` panics, so a failed assertion can't
        // leak the enabled latch into the other serialized bidi tests.
        let _latch = EnabledGuard(is_enabled());
        set_enabled(true);
        f()
    }

    fn paint_plain(line: &str, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        buf.set_line_safe_bidi(0, 0, &Line::from(line.to_string()), width);
        (0..width)
            .map(|x| {
                buf.cell((x, 0))
                    .map(|c| c.symbol())
                    .unwrap_or("")
                    .to_string()
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn disabled_is_identity() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        assert_eq!(visual_text(AR).as_ref(), AR);
        assert!(visual_line(&Line::from(AR)).is_none());
        assert_eq!(paint_plain(&format!("Hi {AR}"), 20), format!("Hi {AR}"));
    }

    #[test]
    fn enabled_reorders_and_keeps_english_leading() {
        with_enabled(|| {
            assert_eq!(visual_text(AR).as_ref(), AR_V);
            assert_eq!(visual_text(FA).as_ref(), FA_V);
            assert_eq!(
                visual_text(&format!("Hello {AR} world")).as_ref(),
                format!("Hello {AR_V} world")
            );
        });
    }

    #[test]
    fn combining_mark_stays_with_base() {
        with_enabled(|| {
            let s = "ب\u{064E}"; // beh + fatha
            let v = visual_text(s);
            assert_eq!(v.as_ref(), s);
            assert!(v.as_ref().contains('\u{064E}'));
        });
    }

    #[test]
    fn mirrors_parens_inside_rtl_run() {
        with_enabled(|| {
            // ا(سلام)ب → reverse RTL run with L4 mirroring → ب(مالس)ا
            let s = format!("ا({AR})ب");
            assert_eq!(visual_text(&s).as_ref(), format!("ب({AR_V})ا"));
        });
    }

    #[test]
    fn strips_bidi_overrides_even_on_latin() {
        with_enabled(|| {
            let s = format!("\u{202E}{AR}");
            assert!(!visual_text(&s).contains('\u{202E}'));
            // RLO + Latin only: still strip the control.
            assert_eq!(visual_text("\u{202E}Hello").as_ref(), "Hello");
        });
    }

    #[test]
    fn table_and_chrome_unchanged_structure() {
        with_enabled(|| {
            let row = format!("│ {AR} │ cell │");
            assert_eq!(visual_text(&row).as_ref(), row);
            assert_eq!(
                visual_text(&format!("│ {AR}")).as_ref(),
                format!("│ {AR_V}")
            );
            assert_eq!(
                visual_text(&format!("• {AR}")).as_ref(),
                format!("• {AR_V}")
            );
            assert_eq!(
                visual_text(&format!("1. {AR}")).as_ref(),
                format!("1. {AR_V}")
            );
        });
    }

    #[test]
    fn shared_paragraph_level_differs_from_per_row() {
        with_enabled(|| {
            let para = format!("{AR} hello {FA}");
            let shared = paragraph_level(&para);
            let row2 = format!("hello {FA}");
            let auto = visual_text_with_level(&row2, paragraph_level(&row2));
            let forced = visual_text_with_level(&row2, shared);
            // Auto base is LTR (leading English); shared base follows the
            // Arabic-first paragraph (RTL), so the Latin/RTL layout differs.
            assert_eq!(auto, format!("hello {FA_V}"));
            assert_ne!(auto, forced);
            assert!(forced.contains(FA_V));
        });
    }

    #[test]
    fn clipboard_slice_is_logical() {
        with_enabled(|| {
            assert_eq!(logical_slice_for_visual_cols(AR, 0, 4), AR);
            // Visual leftmost col is the last logical letter.
            assert_eq!(logical_slice_for_visual_cols(AR, 0, 1), "م");
            let mixed = format!("Hi {AR}");
            assert_eq!(logical_slice_for_visual_cols(&mixed, 3, 7), AR);
        });
    }

    #[test]
    fn logical_cols_map_to_visual_cells() {
        with_enabled(|| {
            // "Hi سلام": logical cols 3..7 are the Arabic run → visual 3..7.
            let mixed = format!("Hi {AR}");
            assert_eq!(logical_cols_to_visual(&mixed, 3, 7), vec![(3, 7)]);
            // Full pure RTL: logical 0..4 maps to the same visual span (reversed glyphs).
            assert_eq!(logical_cols_to_visual(AR, 0, 4), vec![(0, 4)]);
            // Single logical letter at start of AR → rightmost visual cell.
            assert_eq!(logical_cols_to_visual(AR, 0, 1), vec![(3, 4)]);
        });
    }

    #[test]
    fn visual_line_styles() {
        with_enabled(|| {
            let red = Style::default().fg(Color::Red);
            let blue = Style::default().fg(Color::Blue);
            let line = Line::from(vec![Span::styled("Hi ", red), Span::styled(AR, blue)]);
            let visual = visual_line(&line).expect("reorder");
            let flat: String = visual.spans.iter().map(|s| s.content.to_string()).collect();
            assert_eq!(flat, format!("Hi {AR_V}"));
            assert_eq!(visual.spans[0].style.fg, Some(Color::Red));
        });
    }

    #[test]
    fn paint_matches_visual_text_and_column_map() {
        with_enabled(|| {
            let logical = format!("Hi {AR}");
            let painted = paint_plain(&logical, 20);
            assert_eq!(painted, format!("Hi {AR_V}"));
            assert_eq!(painted, visual_text(&logical).as_ref());

            // Map the Arabic logical span; painted cells at those visual cols match AR_V.
            let ranges = logical_cols_to_visual(&logical, 3, 7);
            assert_eq!(ranges, vec![(3, 7)]);
            let (vs, ve) = ranges[0];
            let painted_chars: Vec<char> = painted.chars().collect();
            let cell_slice: String = painted_chars[vs..ve].iter().collect();
            assert_eq!(cell_slice, AR_V);

            // Drag over visual Arabic cells copies logical Arabic.
            assert_eq!(logical_slice_for_visual_cols(&logical, vs, ve), AR);
        });
    }

    #[test]
    fn keeps_zwnj() {
        with_enabled(|| {
            // ZWNJ must survive the strip and travel with its cluster; each
            // joiner clusters with its preceding letter, so reorder reverses
            // beh+ZWNJ, jeem+ZWNJ, dal into dal, jeem+ZWNJ, beh+ZWNJ.
            let with_zwnj = "ب\u{200C}ج\u{200C}د";
            assert_eq!(visual_text(with_zwnj).as_ref(), "دج\u{200C}ب\u{200C}");
        });
    }

    #[test]
    fn visual_col_to_logical_col_inverts_paint() {
        with_enabled(|| {
            // Pure RTL: leftmost visual cell holds the last logical letter.
            assert_eq!(visual_col_to_logical_col(AR, 0), 3);
            assert_eq!(visual_col_to_logical_col(AR, 3), 0);
            // Mixed LTR-leading row: "Hi " keeps its cells, Arabic reverses.
            let mixed = format!("Hi {AR}");
            assert_eq!(visual_col_to_logical_col(&mixed, 0), 0); // 'H'
            assert_eq!(visual_col_to_logical_col(&mixed, 3), 6); // first Arabic cell
            assert_eq!(visual_col_to_logical_col(&mixed, 6), 3); // last Arabic cell
            // Past the end clamps to the logical width.
            assert_eq!(visual_col_to_logical_col(AR, 99), 4);
        });
    }

    #[test]
    fn nested_quote_and_list_marker_stay_left() {
        with_enabled(|| {
            // Blockquote bar + list marker are both chrome: only the body
            // reorders, so paint keeps `│ • ` / `│ 1. ` left-anchored and the
            // column maps (which drop the quote prefix) agree with the body.
            assert_eq!(
                visual_text(&format!("│ • {FA}")).as_ref(),
                format!("│ • {FA_V}")
            );
            assert_eq!(
                visual_text(&format!("│ 1. {FA}")).as_ref(),
                format!("│ 1. {FA_V}")
            );
            // Paint matches, and dropping the quote prefix (the selectable
            // region) still peels the marker so the body reorder is identical.
            assert_eq!(paint_plain(&format!("│ • {FA}"), 20), format!("│ • {FA_V}"));
            assert_eq!(
                visual_text(&format!("• {FA}")).as_ref(),
                format!("• {FA_V}")
            );
        });
    }

    #[test]
    fn control_prefixed_table_row_maps_identity() {
        with_enabled(|| {
            // A leading bidi control must not flip the table classification:
            // paint strips first and leaves the table logical, so the column
            // maps must classify on the stripped string and stay identity.
            let row = "\u{200F}| x | بت |";
            assert_eq!(visual_col_to_logical_col(row, 4), 4);
            assert_eq!(logical_cols_to_visual(row, 2, 6), vec![(2, 6)]);
            assert_eq!(
                logical_slice_for_visual_cols(row, 0, 3),
                slice_display_cols("| x | بت |", 0, 3)
            );
        });
    }

    #[test]
    fn visual_col_to_logical_col_identity_when_disabled() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(false);
        assert_eq!(visual_col_to_logical_col(AR, 2), 2);
        assert_eq!(visual_col_to_logical_col("plain", 3), 3);
    }

    #[test]
    fn set_line_safe_does_not_reorder() {
        with_enabled(|| {
            let area = Rect::new(0, 0, 20, 1);
            let mut buf = Buffer::empty(area);
            buf.set_line_safe(0, 0, &Line::from(AR.to_string()), 20);
            let got: String = (0..4u16)
                .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(""))
                .collect();
            assert_eq!(got, AR);
        });
    }
}
