//! Dropdown list renderer for slash command completion.
//!
//! Renders slash command/arg suggestions as a scrollable list following
//! the same polished layout as the question/answer panel:
//! - Aligned label column (truncated with `...` when too long)
//! - Description text after a fixed gap, truncated to remaining width
//! - Selection highlight (bg_visual + bold on selected row)
//! - Mouse hover highlight (25% blended bg)
//! - Scrollbar when results exceed visible height

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::render::SafeBuf;
use crate::render::line_utils::truncate_str;
use crate::render::scrollbar::render_scrollbar_styled;
use crate::slash::{MAX_VISIBLE_SUGGESTIONS, SlashSnapshot, SuggestionRow};
use crate::theme::Theme;

/// Maximum number of visible rows in the dropdown (excluding separator).
pub const MAX_DROPDOWN_ROWS: u16 = MAX_VISIBLE_SUGGESTIONS as u16;

/// Hard cap on label column width (labels longer than this are truncated).
const LABEL_CAP: usize = 40;

/// Gap (in spaces) between the label column and the description.
const LABEL_DESC_GAP: usize = 2;

/// Prefix display width in columns (`"❯ "` or `"  "`).
const PREFIX_W: usize = 2;

/// Terminal rows needed to show every item at `items_width`, capped at
/// [`MAX_DROPDOWN_ROWS`].
///
/// Items render as flat lines (label + wrapped-description continuations),
/// so an item-count height starves wrapped items and can leave later
/// matches entirely off-area.
pub fn desired_item_rows(items: &[SuggestionRow], items_width: u16) -> u16 {
    if items.is_empty() {
        return 0;
    }
    flat_line_count(items, items_width as usize, MAX_DROPDOWN_ROWS as usize) as u16
}

/// The rendered `" [tag]"` suffix for a row, or `None` when untagged.
fn tag_suffix(row: &SuggestionRow) -> Option<String> {
    row.tag.as_ref().map(|t| format!(" [{t}]"))
}

/// Rendered width of a row's `" [tag]"` suffix (0 when untagged). Measured
/// without allocating: space + `[` + tag + `]` = tag width + 3. The tag shares
/// the label column so descriptions stay aligned across tagged/untagged rows.
fn tag_suffix_width(row: &SuggestionRow) -> usize {
    row.tag.as_ref().map(|t| t.width() + 3).unwrap_or(0)
}

/// Compute the aligned label column width from all visible items.
///
/// The label column gets up to 60% of the available width (capped at `LABEL_CAP`).
/// This prioritises showing the full command name over the description. The tag
/// suffix is folded in so a `/cmd [tag]` row and a plain `/cmd` row share the
/// same description column. Untagged rows keep origin/main behavior (overlong
/// commands are ignored); tagged rows always contribute a `LABEL_CAP`-clamped
/// width so a long tag can never zero out the column.
fn compute_label_column_w(items: &[SuggestionRow], content_w: usize) -> usize {
    let budget = (content_w * 3 / 5).min(LABEL_CAP);
    let max_display_w = items
        .iter()
        .filter_map(|r| {
            let base = r.display.width();
            if r.tag.is_none() {
                (base <= LABEL_CAP).then_some(base)
            } else {
                Some((base + tag_suffix_width(r)).min(LABEL_CAP))
            }
        })
        .max()
        .unwrap_or(0);
    max_display_w.min(budget)
}

/// Build a flat list of styled lines for all visible items.
///
/// Each item produces one or more lines: the first has the prefix + label +
/// first description line; continuation lines are indented to the description
/// column. This is the same approach as `question_view::build_flat_option_lines`.
///
/// Returns `(flat_lines, item_first_line_indices)` where each entry in the
/// second vec is the flat-line index where item `i` starts (for scroll targeting).
fn build_flat_lines(
    items: &[SuggestionRow],
    selected: usize,
    hovered: Option<usize>,
    label_col_w: usize,
    row_w: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let hover_bg = theme.bg_hover;
    let mut flat: Vec<Line<'static>> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        starts.push(flat.len());

        let is_selected = idx == selected;
        let is_hovered = hovered == Some(idx) && !is_selected;
        let row_bg = match crate::views::modal_window::embedded_row_style(theme, is_selected) {
            Some(e) => e.bg,
            None if is_selected => theme.bg_visual,
            None if is_hovered => hover_bg,
            None => theme.bg_light,
        };

        build_item_lines(
            &mut flat,
            item,
            is_selected,
            label_col_w,
            row_w,
            row_bg,
            theme,
        );
    }

    (flat, starts)
}

/// Render the slash dropdown items into the given area.
///
/// This renders ONLY the result rows (no borders or separators).
/// Panel chrome (clear, borders, count hint) is handled by the caller
/// (AgentView). The `area` covers just the item rows.
///
/// `hovered` is the absolute item index currently under the mouse
/// (`None` if no hover). Used for blended hover highlight like file-search.
///
/// Returns the visible-row → item mapping for mouse hit-testing: once a
/// description wraps, rows ≠ items, so callers must not use row arithmetic.
pub fn render_dropdown(
    buf: &mut Buffer,
    area: Rect,
    snap: &SlashSnapshot,
    hovered: Option<usize>,
    theme: &Theme,
) -> RenderedDropdown {
    if area.height == 0 || area.width < 4 || !snap.open {
        return RenderedDropdown::default();
    }

    let items = &snap.matches;
    let selected = snap.selected.min(items.len().saturating_sub(1));

    // Reserve 2 right columns when wrapped content overflows. Decided at
    // full width; narrowing generally adds lines. Dropping a badge can free
    // width — worst case a spare gutter, never a missing scrollbar.
    let content_w = area.width as usize;
    let visible_rows = area.height as usize;
    let needs_scrollbar = flat_line_count(items, content_w, visible_rows + 1) > visible_rows;
    let row_w = if needs_scrollbar {
        content_w.saturating_sub(2)
    } else {
        content_w
    };

    // Compute aligned label column width across all items.
    let label_col_w = compute_label_column_w(items, row_w.saturating_sub(PREFIX_W));

    // Build flat line list (multi-line descriptions produce multiple lines per item).
    let (flat_lines, item_starts) =
        build_flat_lines(items, selected, hovered, label_col_w, row_w, theme);

    // Compute scroll offset so the selected item's first line is visible.
    let selected_start = item_starts.get(selected).copied().unwrap_or(0);
    let total_lines = flat_lines.len();
    let scroll = if total_lines <= visible_rows || selected_start < visible_rows / 2 {
        0
    } else if selected_start + visible_rows / 2 >= total_lines {
        total_lines.saturating_sub(visible_rows)
    } else {
        selected_start.saturating_sub(visible_rows / 2)
    };

    // Render visible slice, recording which item each visible row shows.
    let mut row_items = Vec::with_capacity(visible_rows.min(flat_lines.len()));
    for vis_row in 0..visible_rows {
        let line_idx = scroll + vis_row;
        if line_idx >= flat_lines.len() {
            break;
        }
        row_items.push(
            item_starts
                .partition_point(|&s| s <= line_idx)
                .saturating_sub(1),
        );
        let y = area.y + vis_row as u16;
        let line = &flat_lines[line_idx];
        // Skip rows that fall outside the buffer (resize race).
        if y < buf.area.y || y >= buf.area.bottom() || area.x >= buf.area.right() {
            continue;
        }
        let row_bg = line.style.bg.unwrap_or(theme.bg_light);
        let clamped_w = row_w.min(buf.area.right().saturating_sub(area.x) as usize) as u16;
        let clamped = Rect {
            x: area.x,
            y,
            width: clamped_w,
            height: 1,
        };
        buf.set_style(clamped, Style::default().bg(row_bg));
        buf.set_line_safe(area.x, y, line, row_w as u16);
    }

    // ── Scrollbar ───────────────────────────────────────────────────────
    if needs_scrollbar {
        // Intersect with the frame buffer so a resize race cannot paint past
        // `buf.area` (same failure mode as item rows).
        let sb_x = area.x + area.width.saturating_sub(1);
        let sb_y = area.y.max(buf.area.y);
        let sb_bottom = (area.y.saturating_add(area.height)).min(buf.area.bottom());
        if sb_x < buf.area.right() && sb_bottom > sb_y {
            let scrollbar_area = Rect {
                x: sb_x,
                y: sb_y,
                width: 1,
                height: sb_bottom - sb_y,
            };
            let track_style = Style::default().bg(theme.bg_dark);
            let thumb_style = Style::default().fg(theme.gray_dim).bg(theme.bg_dark);
            render_scrollbar_styled(
                buf,
                Some(scrollbar_area),
                total_lines as u16,
                scrollbar_area.height,
                scroll as u16,
                track_style,
                thumb_style,
            );
        }
    }

    RenderedDropdown {
        row_items,
        has_scrollbar: needs_scrollbar,
    }
}

/// Hit-test geometry produced by [`render_dropdown`].
#[derive(Debug, Clone, Default)]
pub struct RenderedDropdown {
    /// Item index shown on each visible row (top to bottom). Shorter than the
    /// area height when the content ends early.
    pub row_items: Vec<usize>,
    /// Whether the right 2 columns of the area are the scrollbar gutter.
    pub has_scrollbar: bool,
}

/// Badge geometry shared by [`flat_line_count`] and [`build_item_lines`]
/// so height estimation cannot diverge from what is drawn.
struct BadgeLayout {
    badge: Option<String>,
    desc_w: usize,
}

impl BadgeLayout {
    fn compute(item: &SuggestionRow, total_w: usize, desc_indent: usize) -> Self {
        let badge = item
            .provenance
            .as_ref()
            .map(|p| p.badge().into_owned())
            .filter(|badge| desc_indent + 1 + badge.width() < total_w);
        let reserve = badge.as_ref().map_or(0, |badge| 1 + badge.width());
        let desc_w = total_w
            .saturating_sub(desc_indent)
            .saturating_sub(reserve)
            .max(1);
        Self { badge, desc_w }
    }
}

/// Flat line count of `items` at `row_w`, mirroring [`build_item_lines`]
/// (label line + wrapped-description continuation lines). Saturates at
/// `cap` so the empty-query dropdown (every command listed) doesn't wrap
/// hundreds of descriptions just to compare against a single-digit height.
fn flat_line_count(items: &[SuggestionRow], row_w: usize, cap: usize) -> usize {
    let label_col_w = compute_label_column_w(items, row_w.saturating_sub(PREFIX_W));
    let desc_indent = PREFIX_W + label_col_w + LABEL_DESC_GAP;
    let mut lines = 0usize;
    for item in items {
        let desc_w = BadgeLayout::compute(item, row_w, desc_indent).desc_w;
        lines += if item.description.is_empty() {
            1
        } else {
            simple_word_wrap(&item.description, desc_w).len()
        };
        if lines >= cap {
            return cap;
        }
    }
    lines
}

/// Build lines for a single dropdown item and append them to `out`.
///
/// Layout (same as question view):
/// - First line:  `❯ /command-name  First line of description`
/// - Continuation: `                 Wrapped description text`
///   ^indent aligned to description column
#[allow(clippy::too_many_arguments)]
fn build_item_lines(
    out: &mut Vec<Line<'static>>,
    item: &SuggestionRow,
    is_selected: bool,
    label_col_w: usize,
    total_w: usize,
    row_bg: ratatui::style::Color,
    theme: &Theme,
) {
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let embed = crate::views::modal_window::embedded_row_style(theme, is_selected);
    let primary_fg = embed.map_or(theme.text_primary, |e| e.fg(theme.text_primary));
    let match_fg = embed.map_or(theme.fuzzy_accent, |e| e.fg(theme.fuzzy_accent));
    let desc_fg = embed.map_or(theme.gray, |e| e.fg(theme.gray));
    let normal_style = Style::default()
        .fg(primary_fg)
        .bg(row_bg)
        .add_modifier(bold);
    let match_style = Style::default().fg(match_fg).bg(row_bg).add_modifier(bold);
    let desc_style = Style::default().fg(desc_fg).bg(row_bg);
    let bg_style = Style::default().bg(row_bg);
    let tag_style = Style::default().fg(theme.accent_system).bg(row_bg);

    // 1. Build prefix + label spans with fuzzy match highlighting.
    let prefix = if is_selected {
        crate::glyphs::prompt_arrow()
    } else {
        "  "
    };
    let prefix_span = Span::styled(
        prefix.to_string(),
        if is_selected { normal_style } else { bg_style },
    );

    // Optional " [tag]" suffix, right-aligned at the end of the label column
    // (just left of the description). Truncated/reserved so the name never
    // overruns it at narrow widths. Leading space in the suffix separates
    // label and tag when padding is 0 (longest command+tag row).
    let tag_text = tag_suffix(item).map(|s| truncate_str(&s, label_col_w));
    let tag_w = tag_text.as_deref().map(|s| s.width()).unwrap_or(0);

    let label = truncate_str(&item.display, label_col_w.saturating_sub(tag_w));
    let label_w = label.width();
    let padding = label_col_w.saturating_sub(label_w + tag_w);

    // Build per-character spans for the label with fuzzy highlight.
    let label_spans = build_highlighted_spans(&label, &item.indices, normal_style, match_style);

    // Description column indent (prefix + label + gap). `label_col_w` already
    // includes the tag suffix, so descriptions align across tagged/untagged
    // rows.
    let desc_indent = PREFIX_W + label_col_w + LABEL_DESC_GAP;
    let layout = BadgeLayout::compute(item, total_w, desc_indent);

    // Word-wrap description into lines of `desc_w` width.
    let desc_lines = if item.description.is_empty() {
        Vec::new()
    } else {
        simple_word_wrap(&item.description, layout.desc_w)
    };

    // 2. First line: prefix + label + padding + [tag] + gap + first desc + right badge.
    {
        let mut spans = vec![prefix_span];
        spans.extend(label_spans);
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), bg_style));
        }
        if let Some(tag_text) = tag_text {
            spans.push(Span::styled(tag_text, tag_style));
        }
        let first_desc = desc_lines.first().map(String::as_str).unwrap_or("");
        if !first_desc.is_empty() {
            spans.push(Span::styled(" ".to_string(), bg_style));
            spans.push(Span::styled(first_desc.to_string(), desc_style));
        }
        if let Some(badge) = &layout.badge {
            // Align to the first line's used width, not wrap `desc_indent`.
            let desc_gap = if first_desc.is_empty() { 0 } else { 1 };
            let used = PREFIX_W + label_col_w + desc_gap + first_desc.width();
            let gap = total_w.saturating_sub(used).saturating_sub(badge.width());
            if gap > 0 {
                spans.push(Span::styled(" ".repeat(gap), bg_style));
            }
            spans.push(Span::styled(badge.clone(), desc_style));
        }
        out.push(Line::from(spans).style(bg_style));
    }

    // 3. Continuation lines: indented to description column.
    for desc_line in desc_lines.iter().skip(1) {
        let spans = vec![
            Span::styled(" ".repeat(desc_indent), bg_style),
            Span::styled(desc_line.clone(), desc_style),
        ];
        out.push(Line::from(spans).style(bg_style));
    }
}

/// Build spans for a text string with fuzzy match character highlighting.
///
/// Characters at positions listed in `indices` get `match_style` (accent color),
/// all others get `normal_style`. Adjacent characters with the same style are
/// coalesced into a single `Span` to keep the span count low.
fn build_highlighted_spans(
    text: &str,
    indices: &[u32],
    normal_style: Style,
    match_style: Style,
) -> Vec<Span<'static>> {
    if indices.is_empty() {
        return vec![Span::styled(text.to_string(), normal_style)];
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = String::new();
    let mut current_is_match = false;
    let mut idx_iter = indices.iter().copied().peekable();

    for (char_idx, ch) in text.chars().enumerate() {
        let is_match = idx_iter.peek() == Some(&(char_idx as u32));
        if is_match {
            idx_iter.next();
        }

        if char_idx == 0 {
            current_is_match = is_match;
            current.push(ch);
        } else if is_match == current_is_match {
            current.push(ch);
        } else {
            // Style transition — flush current run.
            let style = if current_is_match {
                match_style
            } else {
                normal_style
            };
            spans.push(Span::styled(std::mem::take(&mut current), style));
            current_is_match = is_match;
            current.push(ch);
        }
    }

    if !current.is_empty() {
        let style = if current_is_match {
            match_style
        } else {
            normal_style
        };
        spans.push(Span::styled(current, style));
    }

    spans
}

/// Simple word-wrap for plain text. Returns lines of at most `width` chars.
///
/// Breaks at word boundaries when possible, hard-breaks at `width` otherwise.
fn simple_word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    // Normalize: collapse newlines into spaces.
    let normalized = text.replace('\n', " ");
    let mut remaining = normalized.as_str();
    while !remaining.is_empty() {
        if remaining.width() <= width {
            lines.push(remaining.to_string());
            break;
        }
        // Find break point: last space within width, or hard break.
        let break_at = {
            let mut last_space = None;
            let mut w = 0;
            for (i, ch) in remaining.char_indices() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + cw > width {
                    break;
                }
                w += cw;
                if ch == ' ' {
                    last_space = Some(i);
                }
            }
            // Prefer word boundary; fall back to hard break at width.
            last_space.map(|i| i + 1).unwrap_or_else(|| {
                remaining
                    .char_indices()
                    .scan(0usize, |w, (i, ch)| {
                        *w += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        if *w > width {
                            None
                        } else {
                            Some(i + ch.len_utf8())
                        }
                    })
                    .last()
                    .unwrap_or(remaining.len())
            })
        };
        let (chunk, rest) = remaining.split_at(break_at);
        lines.push(chunk.trim_end().to_string());
        remaining = rest.trim_start();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::CommandProvenance;

    #[test]
    fn desired_item_rows_caps_many_short_items() {
        let matches: Vec<SuggestionRow> = (0..20)
            .map(|i| SuggestionRow {
                display: format!("/cmd{i}"),
                description: String::new(),
                insert_text: format!("/cmd{i}"),
                indices: vec![],
                tag: None,
                provenance: None,
            })
            .collect();
        assert_eq!(desired_item_rows(&matches, 80), MAX_DROPDOWN_ROWS);
        assert_eq!(desired_item_rows(&[], 80), 0);
    }

    /// During terminal resize the computed items area can extend past
    /// the frame buffer. Item paint must not panic via ratatui `set_line`.
    #[test]
    fn render_dropdown_past_buffer_bottom_does_not_panic() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let matches: Vec<SuggestionRow> = (0..12)
            .map(|i| SuggestionRow {
                display: format!("/cmd{i}"),
                description: format!("description for command {i}"),
                insert_text: format!("/cmd{i}"),
                indices: vec![],
                tag: None,
                provenance: None,
            })
            .collect();
        let snap = SlashSnapshot {
            open: true,
            matches,
            selected: 0,
            ..Default::default()
        };

        // 80×10 buffer; items area starts at y=8 with height 8 → rows y=8..15,
        // which extends past the buffer bottom (y=10). Mimics a resize race
        // where layout still thinks the terminal is taller than the buffer.
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        let area = Rect::new(2, 8, 76, 8);
        render_dropdown(&mut buf, area, &snap, Some(1), &theme);
    }

    #[test]
    fn fuzzy_indices_render_with_theme_accent() {
        let theme = Theme::default();
        let normal = Style::default().fg(theme.text_primary);
        let matched = Style::default().fg(theme.fuzzy_accent);
        let spans = build_highlighted_spans("ssh-wrap", &[0, 1, 2], normal, matched);
        assert_eq!(spans[0].content.as_ref(), "ssh");
        assert_eq!(spans[0].style.fg, Some(theme.fuzzy_accent));
        assert_eq!(spans[1].style.fg, Some(theme.text_primary));
    }

    #[test]
    fn provenance_badge_is_right_aligned() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let matches = vec![
            SuggestionRow {
                display: "/login".into(),
                description: "Log in or re-authenticate with your account".into(),
                insert_text: "/login".into(),
                indices: vec![],
                tag: None,
                provenance: Some(CommandProvenance::Builtin),
            },
            SuggestionRow {
                display: "/acme:login".into(),
                description: "Acme account login".into(),
                insert_text: "/acme:login ".into(),
                indices: vec![],
                tag: None,
                provenance: Some(CommandProvenance::Skill {
                    source: "acme".to_string(),
                }),
            },
        ];
        let snap = SlashSnapshot {
            open: true,
            matches,
            selected: 0,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        render_dropdown(&mut buf, area, &snap, None, &theme);

        let line0: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let line1: String = (0..80).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(line0.contains("Log in or re-authenticate"));
        assert!(line1.contains("Acme account login"));
        assert!(!line0.contains(" · built-in"));
        // Flush right: the badge's last glyph sits in the final column.
        assert!(line0.ends_with("built-in"), "not right-aligned: {line0:?}");
        assert!(
            line1.ends_with("skill · acme"),
            "not right-aligned: {line1:?}"
        );
    }

    fn row(display: &str, description: &str) -> SuggestionRow {
        SuggestionRow {
            display: display.into(),
            description: description.into(),
            insert_text: display.into(),
            indices: vec![],
            tag: None,
            provenance: None,
        }
    }

    /// Degenerate geometry sweep: tiny/zero widths and heights, over-wide
    /// glyphs, and unbreakable words must neither panic (debug arithmetic,
    /// non-char-boundary splits) nor loop (zero-progress wrap).
    #[test]
    fn tiny_geometry_never_panics_or_hangs() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let nasty = vec![
            row(
                "/a",
                "one-unbreakable-word-ที่ยาวมาก-🦀🦀🦀-with-no-spaces-at-all",
            ),
            row(
                "/日本語コマンド",
                "全角文字だけで構成された説明文です、スペースなし。",
            ),
            row("/b", ""),
        ];
        for width in 0..=8u16 {
            assert!(desired_item_rows(&nasty, width) <= MAX_DROPDOWN_ROWS);
            for height in 0..=3u16 {
                let snap = SlashSnapshot {
                    open: true,
                    matches: nasty.clone(),
                    selected: 2,
                    ..Default::default()
                };
                let mut buf = Buffer::empty(Rect::new(0, 0, width.max(1), height.max(1)));
                let area = Rect::new(0, 0, width, height);
                let rendered = render_dropdown(&mut buf, area, &snap, Some(0), &theme);
                assert!(rendered.row_items.len() <= height as usize);
            }
        }
    }

    /// Two matches, first description wraps: sizing must count wrapped
    /// lines or the sibling lands off-area.
    #[test]
    fn desired_item_rows_counts_wrapped_description_lines() {
        let long = "Apply the Japandi visual design system - a warm, earthy, calm aesthetic \
                    that merges Japanese restraint with Scandinavian comfort - when building \
                    HTML artifacts, web pages, UI mockups, components.";
        let items = vec![
            row("/japandi", long),
            row("/japandi2", "Japandi v2 system."),
        ];
        let rows = desired_item_rows(&items, 60);
        assert!(
            rows > items.len() as u16,
            "wrapped lines must exceed item count, got {rows}"
        );
        assert!(rows <= MAX_DROPDOWN_ROWS);

        // All-short descriptions keep the one-row-per-item sizing.
        let short = vec![row("/exit", "Quit"), row("/model", "Switch model")];
        assert_eq!(desired_item_rows(&short, 60), 2);
    }

    /// Every item is on screen (present in the hit map) when the area is
    /// sized via `desired_item_rows`.
    #[test]
    fn render_dropdown_row_map_covers_all_items_at_desired_height() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let long = "Apply the Japandi visual design system - a warm, earthy, calm aesthetic \
                    that merges Japanese restraint with Scandinavian comfort.";
        let matches = vec![
            row("/japandi", long),
            row("/japandi2", "Japandi v2 system."),
        ];
        let width: u16 = 60;
        let rows = desired_item_rows(&matches, width);
        let snap = SlashSnapshot {
            open: true,
            matches,
            selected: 0,
            ..Default::default()
        };

        let mut buf = Buffer::empty(Rect::new(0, 0, width, rows + 2));
        let area = Rect::new(0, 0, width, rows);
        let rendered = render_dropdown(&mut buf, area, &snap, None, &theme);

        assert_eq!(rendered.row_items.len(), rows as usize);
        assert!(
            rendered.row_items.contains(&0) && rendered.row_items.contains(&1),
            "both items must be on screen at scroll 0: {:?}",
            rendered.row_items
        );
        assert!(!rendered.has_scrollbar, "content fits; no scrollbar");
        // Rows are monotone and grouped: item 1 starts after item 0's lines.
        assert!(rendered.row_items.windows(2).all(|w| w[0] <= w[1]));
    }

    /// Scrollbar + row map when content exceeds the capped height.
    #[test]
    fn render_dropdown_scrollbar_on_line_overflow() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let long = "A deliberately verbose description that will wrap across several \
                    lines at sixty columns to overflow the capped dropdown height.";
        let matches: Vec<SuggestionRow> = (0..4).map(|i| row(&format!("/cmd{i}"), long)).collect();
        let width: u16 = 60;
        let rows = desired_item_rows(&matches, width);
        assert_eq!(rows, MAX_DROPDOWN_ROWS, "content must exceed the cap");

        let snap = SlashSnapshot {
            open: true,
            matches,
            selected: 0,
            ..Default::default()
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, width, rows + 2));
        let area = Rect::new(0, 0, width, rows);
        let rendered = render_dropdown(&mut buf, area, &snap, None, &theme);
        assert!(rendered.has_scrollbar, "overflowing lines need a scrollbar");
        assert_eq!(rendered.row_items.len(), rows as usize);
        assert_eq!(rendered.row_items[0], 0, "scroll starts at the top");
    }

    /// A tagged row renders "[tag]" (system-accent) between the command name and
    /// the description; untagged rows and arg rows render no bracket.
    #[test]
    fn tagged_command_row_renders_bracketed_tag() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::default();
        let mut tagged = row("/tagged", "does work");
        tagged.tag = Some("new".to_string());
        let untagged = row("/plain", "no tag here");
        let arg = row("argrow", "an argument"); // arg rows always have tag = None

        let width: u16 = 60;
        let snap = SlashSnapshot {
            open: true,
            // Select row 1 so the tagged + arg rows stay unselected.
            matches: vec![tagged, untagged, arg],
            selected: 1,
            ..Default::default()
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 3));
        let area = Rect::new(0, 0, width, 3);
        render_dropdown(&mut buf, area, &snap, None, &theme);

        let row_text = |y: u16| -> String {
            (0..width)
                .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect()
        };

        // True buffer column of `needle`'s first cell. Do not use `str::find` on
        // `row_text`: the selected-row prefix is multi-byte (`❯`), so byte
        // offsets drift from display columns and falsely report misalignment.
        let desc_col = |y: u16, needle: &str| -> u16 {
            let needle_chars: Vec<char> = needle.chars().collect();
            (0..width)
                .find(|&start| {
                    needle_chars.iter().enumerate().all(|(i, ch)| {
                        let x = start + i as u16;
                        x < width
                            && buf
                                .cell((x, y))
                                .is_some_and(|c| c.symbol() == ch.to_string())
                    })
                })
                .unwrap_or_else(|| panic!("row {y} missing {needle:?}: {}", row_text(y)))
        };

        // Row 0 (tagged): "[new]" present, and the open-bracket cell uses accent.
        assert!(
            row_text(0).contains("[new]"),
            "tagged row shows [new]: {}",
            row_text(0)
        );
        let bracket_x = (0..width)
            .find(|&x| buf.cell((x, 0)).map(|c| c.symbol()) == Some("["))
            .expect("open bracket in tagged row");
        assert_eq!(
            buf.cell((bracket_x, 0)).unwrap().fg,
            theme.accent_system,
            "tag renders in the system accent"
        );

        // Row 1 (untagged) and row 2 (arg): no bracket at all.
        assert!(
            !row_text(1).contains('['),
            "untagged row has no bracket: {}",
            row_text(1)
        );
        assert!(
            !row_text(2).contains('['),
            "arg row has no bracket: {}",
            row_text(2)
        );

        // Shared-column invariant: the description starts at the same buffer
        // column on the tagged row and the untagged row (the tag folds into
        // the label column, so it never shifts the description).
        let desc0_x = desc_col(0, "does work");
        let desc1_x = desc_col(1, "no tag here");
        assert_eq!(
            desc0_x,
            desc1_x,
            "tagged and untagged descriptions must share the same column (row0={}, row1={})",
            row_text(0),
            row_text(1)
        );

        // Tag is right-aligned: closing `]` sits at the label-column right edge,
        // immediately before the first-line gap space and then the description.
        // First-line gap is one space (see build_item_lines), so `]` column ==
        // desc_col - 1 - 1. (Do not use str::find — multi-byte selected prefix.)
        let close_bracket_x = (0..width)
            .rev()
            .find(|&x| buf.cell((x, 0)).map(|c| c.symbol()) == Some("]"))
            .expect("closing ] on tagged row");
        assert_eq!(
            close_bracket_x,
            desc0_x - 1 - 1,
            "tag right-aligned: ] should sit just left of the desc gap (row0={})",
            row_text(0)
        );

        // A long tag at narrow widths must truncate without panicking (zero-width
        // / non-char-boundary math), including the width < 4 early-return path.
        let mut long_tagged = row("/x", "d");
        long_tagged.tag = Some("superlongtagname".to_string());
        let narrow = SlashSnapshot {
            open: true,
            matches: vec![long_tagged],
            selected: 0,
            ..Default::default()
        };
        for w in 0..=12u16 {
            let mut nb = Buffer::empty(Rect::new(0, 0, w.max(1), 1));
            let na = Rect::new(0, 0, w, 1);
            let _ = render_dropdown(&mut nb, na, &narrow, None, &theme);
        }
    }
}
