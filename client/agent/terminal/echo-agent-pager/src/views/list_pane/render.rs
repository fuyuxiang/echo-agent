//! `ListPane<'a, T>` — the rendering widget for a scrollable list pane.
//!
//! This is a [`StatefulWidget`] that borrows item data and renders the visible
//! portion based on [`ListPaneState`]'s layout cache, scroll position, and
//! selection.  Includes an optional scrollbar when content overflows.
//!
//! ## Rendering Pipeline
//!
//! 1. Caller calls `state.prepare_layout(items, width, viewport_height)` once per
//!    frame (computes layout cache, resolves selection IDs → indices, clamps scroll).
//! 2. Caller constructs `ListPane::new(items, &state)` and calls
//!    `StatefulWidget::render(...)` or `render_ref(...)`.
//! 3. This module iterates only the visible range (from `state.visible_range()`),
//!    maps visible → physical indices, and delegates to `ListItem::render()`.
//! 4. Scrollbar is rendered when content overflows the viewport.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidget;

use super::layout::WrapMode;
use super::state::ListPaneState;
use super::{ListItem, ListPaneStyle};
use crate::render::SafeBuf;
use crate::render::highlight::paint_match_highlights;
use crate::render::scrollbar::{maybe_split_for_scrollbar, render_scrollbar_styled};

/// Rendering widget for a scrollable list pane.
///
/// Borrows item data (`&'a [T]`) and renders the visible portion according
/// to the pre-computed layout in [`ListPaneState`].
///
/// ## Rendering Pipeline (post-passes)
///
/// For each visible item, the framework applies overlays in order:
/// 1. **Item content** — `ListItem::render()` paints text/chrome.
/// 2. **Selection bg** — framework overlays `selection_bg` on the selected row(s).
/// 3. **Match highlight** — inverts fg/bg on matched cells (style inversion).
/// 4. **Truncation ellipsis** — `…` on the last row if the item was truncated.
///
/// Items do **not** paint selection or match backgrounds themselves.
///
/// ## Usage
///
/// ```ignore
/// // 1. Prepare layout (once per frame, before rendering)
/// state.prepare_layout(&items, content_width, viewport_height);
///
/// // 2. Render
/// let pane = ListPane::new(&items).focused(true);
/// StatefulWidget::render(pane, area, buf, &mut state);
/// ```
pub struct ListPane<'a, T: ListItem> {
    /// The full (unfiltered) item slice from the model.
    items: &'a [T],
    /// Whether this pane has keyboard focus (passed to `ListItem::render`).
    focused: bool,
    /// Visual style for framework-level overlays (selection, match highlights).
    style: ListPaneStyle,
}

impl<'a, T: ListItem> ListPane<'a, T> {
    /// Create a new `ListPane` widget borrowing the given items.
    pub fn new(items: &'a [T]) -> Self {
        Self {
            items,
            focused: false,
            style: ListPaneStyle::default(),
        }
    }

    /// Set whether this pane has keyboard focus.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Set the visual style for selection/highlight overlays.
    pub fn style(mut self, style: ListPaneStyle) -> Self {
        self.style = style;
        self
    }
}

impl<T: ListItem> StatefulWidget for ListPane<'_, T> {
    type State = ListPaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Split off bottom row(s) when the input bar is open or a matcher is
        // active. `bottom_bar_height` returns 0 when no bar is shown, and the
        // bar height (1, or up to 5 for multi-line comment mode) otherwise.
        let bar_height = state.bottom_bar_height(area.height);
        let (list_area, bottom_bar_area) = if bar_height > 0 {
            let list = Rect {
                height: area.height.saturating_sub(bar_height),
                ..area
            };
            let bar = Rect {
                y: area.y + list.height,
                height: bar_height,
                ..area
            };
            (list, Some(bar))
        } else {
            (area, None)
        };

        let total_height = state.total_height();
        let viewport_height = list_area.height;

        // Scale down for scrollbar when total_height exceeds u16::MAX.
        // Both total and offset are divided by the same factor so the thumb
        // position remains proportionally correct.
        let scale = if total_height > u16::MAX as usize {
            (total_height / u16::MAX as usize) + 1
        } else {
            1
        };
        let scaled_total = (total_height / scale) as u16;
        let scaled_offset = (state.scroll_offset() / scale) as u16;

        // Split area for scrollbar if content overflows.
        let (content_area, scrollbar_area) = maybe_split_for_scrollbar(list_area, scaled_total);

        // Store scrollbar area for click/scroll hit-testing.
        state.set_scrollbar_area(scrollbar_area);

        // Render items into the content area.
        self.render_items(content_area, buf, state);

        // Render corner overlay indicators.
        render_corner_indicators(content_area, buf, state, &self.style);

        // Render "Copied!" toast (bottom-right corner, briefly after y-copy).
        // Rendered AFTER indicators so we can skip the bottom-right indicator
        // to avoid overlapping.
        if state.copy_toast_active() && content_area.height > 0 && content_area.width > 8 {
            let toast_text = " Copied!";
            let x = content_area.right().saturating_sub(toast_text.len() as u16);
            let y = content_area.bottom().saturating_sub(1);
            // Write each char, keeping bg (selection highlight) but
            // overriding fg + modifiers so content styles don't leak.
            for (i, ch) in toast_text.chars().enumerate() {
                let cell = &mut buf[(x + i as u16, y)];
                cell.set_char(ch);
                cell.fg = self.style.toast_fg;
                cell.modifier = ratatui::style::Modifier::BOLD;
            }
        }

        // Render scrollbar with style colors.
        let track_style = Style::default().bg(self.style.scrollbar_bg);
        let thumb_style = Style::default()
            .fg(self.style.scrollbar_fg)
            .bg(self.style.scrollbar_bg);
        render_scrollbar_styled(
            buf,
            scrollbar_area,
            scaled_total,
            viewport_height,
            scaled_offset,
            track_style,
            thumb_style,
        );

        // Render bottom bar (input bar when editing, status when accepted).
        if let Some(bar_area) = bottom_bar_area {
            render_bottom_bar(bar_area, buf, state, &self.style);
        }
    }
}

impl<T: ListItem> ListPane<'_, T> {
    /// Render a single item using the content/prefix framework.
    ///
    /// Handles both NoWrap (single row, truncated) and Wrap (word-wrapped
    /// with prefix indentation on continuation lines).
    fn render_item_framework(
        item: &T,
        area: Rect,
        buf: &mut Buffer,
        wrap_mode: WrapMode,
        is_selected: bool,
        is_cursor: bool,
    ) {
        use super::line_display_width;
        use crate::render::wrapping::word_wrap_line;

        // Fill full-width background if the item specifies one (e.g., code blocks).
        if let Some(bg) = item.background() {
            let bg_style = ratatui::style::Style::default().bg(bg);
            buf.set_style(area, bg_style);
        }

        let content = item.content();
        let prefix = if is_cursor {
            item.prefix_cursor()
        } else if is_selected {
            item.prefix_in_selection()
        } else {
            item.prefix()
        };
        let prefix_w = prefix.as_ref().map(|p| line_display_width(p)).unwrap_or(0) as u16;

        // Paint prefix on row 0.
        if let Some(ref pfx) = prefix {
            buf.set_line_safe(area.x, area.y, pfx, prefix_w);
        }

        if area.height == 1 || wrap_mode == WrapMode::NoWrap {
            // NoWrap / single row: paint content after prefix, truncated.
            let content_x = area.x + prefix_w;
            let content_w = area.width.saturating_sub(prefix_w);
            if content_w > 0 {
                // Content paint is bidi-aware; search highlights remap columns.
                buf.set_line_safe_bidi(content_x, area.y, content, content_w);
            }
        } else {
            // Wrap: word-wrap content into (area.width - prefix_w) columns.
            let content_w = area.width.saturating_sub(prefix_w) as usize;
            if content_w == 0 {
                return;
            }
            let wrapped = word_wrap_line(content, content_w);
            let content_x = area.x + prefix_w;
            for (i, wl) in wrapped.iter().enumerate() {
                let y = area.y + i as u16;
                if y >= area.y + area.height {
                    break;
                }
                buf.set_line_safe_bidi(content_x, y, wl, content_w as u16);
                // On continuation lines (i > 0), the prefix area is left
                // blank — indentation happens via the column offset.
            }
        }
    }

    /// Render the visible items into the content area.
    fn render_items(&self, area: Rect, buf: &mut Buffer, state: &ListPaneState) {
        let visible = state.visible_range();
        if visible.is_empty() {
            return;
        }

        let skip_rows = state.first_item_skip_rows();
        let selected_vi = state.selected_index();
        let multi_range = state.multi_range();
        let first_vi = visible.start;
        let wrap_mode = state.wrap_mode();

        let mut cursor_y = area.y;
        let viewport_bottom = area.y + area.height;

        for vi in visible {
            if cursor_y >= viewport_bottom {
                break;
            }

            let pi = state.to_physical(vi);
            // After prepare_layout refilter; skip rather than panic in release.
            debug_assert!(pi < self.items.len());
            let Some(item) = self.items.get(pi) else {
                continue;
            };
            let item_h = state.layout().item_height(vi);

            // How many rows to skip at the top of this item (only for the first item).
            let skip = if vi == first_vi { skip_rows } else { 0 };

            // How many rows of this item are actually visible.
            let visible_h = item_h.saturating_sub(skip);
            let rows_available = viewport_bottom.saturating_sub(cursor_y);
            let rows_to_render = visible_h.min(rows_available);

            if rows_to_render == 0 {
                continue;
            }

            // Is this item selected (cursor or visual range)?
            let is_cursor = selected_vi == Some(vi);
            let is_selected = is_cursor
                || multi_range
                    .as_ref()
                    .map(|r| r.contains(&vi))
                    .unwrap_or(false);

            // Dispatch: content-based (framework) vs custom render.
            let uses_framework = !item.content().spans.is_empty();

            // If the item is fully visible (no clipping), render directly.
            // If partially visible (skip > 0 or truncated at bottom), render
            // into a scratch area and blit the visible portion.
            if skip == 0 && rows_to_render == item_h {
                // Fast path: render directly into buf.
                let item_area = Rect {
                    x: area.x,
                    y: cursor_y,
                    width: area.width,
                    height: item_h,
                };
                if uses_framework {
                    Self::render_item_framework(
                        item,
                        item_area,
                        buf,
                        wrap_mode,
                        is_selected,
                        is_cursor,
                    );
                } else {
                    item.render(item_area, buf, is_selected, self.focused);
                }
            } else {
                // Slow path: render into a temp buffer, then blit the visible rows.
                let full_area = Rect {
                    x: 0,
                    y: 0,
                    width: area.width,
                    height: item_h,
                };
                let mut scratch = Buffer::empty(full_area);
                if uses_framework {
                    Self::render_item_framework(
                        item,
                        full_area,
                        &mut scratch,
                        wrap_mode,
                        is_selected,
                        is_cursor,
                    );
                } else {
                    item.render(full_area, &mut scratch, is_selected, self.focused);
                }

                // Copy visible rows from scratch to buf.
                // Preserve the destination's bg when the source cell has
                // the default (Reset) background — the parent (e.g., a popup
                // overlay) may have set a specific bg on the area, and
                // Buffer::empty starts with Reset which would erase it.
                for row in 0..rows_to_render {
                    let src_y = skip + row;
                    let dst_y = cursor_y + row;
                    for col in 0..area.width {
                        let src_cell = &scratch[(col, src_y)];
                        let dst_cell = &mut buf[(area.x + col, dst_y)];
                        let parent_bg = dst_cell.bg;
                        *dst_cell = src_cell.clone();
                        if dst_cell.bg == ratatui::style::Color::Reset {
                            dst_cell.bg = parent_bg;
                        }
                    }
                }
            }

            // --- Post-pass 1: Selection background overlay ---
            // Patches only the bg of each cell, preserving fg, content, and
            // modifiers.  Applied after item render so items don't need to
            // know about selection colors.
            // Shown when focused, or when `show_selection_when_unfocused` is set.
            let show_sel = self.focused || state.show_selection_when_unfocused();
            if is_selected && show_sel {
                // Use different bg for visual range vs cursor line.
                // When `uniform_visual_bg` is set, the cursor line blends
                // into the visual range (distinguished by prefix only).
                let in_visual = state.visual_mode;
                let bg = if is_cursor && !(in_visual && self.style.uniform_visual_bg) {
                    self.style.selection_bg
                } else {
                    self.style.visual_select_bg
                };
                let sel_area = Rect {
                    x: area.x,
                    y: cursor_y,
                    width: area.width,
                    height: rows_to_render,
                };
                buf.set_style(sel_area, Style::default().bg(bg));
            }

            // --- Post-pass 2: Match highlight overlay ---
            // Invert (REVERSED) the cells covering each match of the active
            // query.  Gated on `show_highlights` so callers can suppress the
            // overlay (e.g. after accepting a filter, where every line matches).
            if state.show_highlights
                && let Some(matcher) = state.matcher()
            {
                let single_row = wrap_mode == WrapMode::NoWrap || item_h == 1;
                // Highlights must map the painted string. Framework items paint
                // `content()` reordered, so highlight over it (not `search_text()`,
                // which can differ in base direction / chrome). Custom `render()`
                // items paint logically and keep logical columns.
                let content_plain = (uses_framework && crate::render::bidi::is_enabled())
                    .then(|| crate::scrollback::types::line_plain_text(item.content()));
                let (hl_text, map_visual) = match &content_plain {
                    Some(text) => (text.as_str(), true),
                    None => (item.search_text(), false),
                };
                paint_match_highlights(
                    buf,
                    area,
                    cursor_y,
                    viewport_bottom,
                    skip,
                    item.search_text_col_offset(),
                    hl_text,
                    matcher.compiled_regex(),
                    single_row,
                    map_visual,
                );
            }

            // --- Post-pass 3: Truncation ellipsis ---
            // If the item's full wrapped height exceeds its allocated layout
            // height, place "…" on the last rendered row.  This only triggers
            // in NoWrap mode (where item_h == 1 regardless of content length).
            // Viewport clipping does NOT trigger this — only true text truncation.
            if item.desired_height(area.width) > item_h && rows_to_render > 0 {
                let last_y = cursor_y + rows_to_render - 1;
                render_truncation_ellipsis(buf, last_y, area.x, area.width);
            }

            cursor_y += rows_to_render;
        }
    }
}

// ---------------------------------------------------------------------------
// Truncation ellipsis
// ---------------------------------------------------------------------------

/// Place a `…` at the end of text on row `y` to indicate truncation.
///
/// Scans from right to left for the rightmost non-space cell.  If there is
/// room after it (text doesn't fill the full width), the `…` is appended.
/// If the text fills the exact width, the last character is replaced — this
/// matches the convention in VS Code, `less`, `bat`, and Vim.
///
/// The `…` inherits the `fg` color from the adjacent text cell and preserves
/// the cell's existing `bg` (e.g., selection highlight).
fn render_truncation_ellipsis(buf: &mut Buffer, y: u16, x_start: u16, width: u16) {
    if width == 0 {
        return;
    }

    let x_end = x_start + width; // exclusive

    // Find rightmost non-space cell.
    let mut last_text_x: Option<u16> = None;
    for x in (x_start..x_end).rev() {
        if buf[(x, y)].symbol() != " " {
            last_text_x = Some(x);
            break;
        }
    }

    let (ellipsis_x, donor_x) = match last_text_x {
        Some(x) if x + 1 < x_end => (x + 1, x), // append after text, inherit from text
        Some(x) => (x, x),                      // replace last char, keep its style
        None => return,                         // entire row is blank
    };

    // Inherit fg from the donor cell, preserve bg of the target cell.
    let fg = buf[(donor_x, y)].fg;
    let cell = &mut buf[(ellipsis_x, y)];
    cell.set_symbol("…");
    cell.fg = fg;
}

// ---------------------------------------------------------------------------
// Corner overlay indicators
// ---------------------------------------------------------------------------

/// Render single-character corner indicators for scroll position / follow mode.
///
/// - Top-right: `▲` (dim) when content is scrolled down (more above).
/// - Bottom-right: `◆` (dim) in follow mode, `▼` (dim) when more content below,
///   or nothing when at the bottom in NAV mode.
fn render_corner_indicators(
    area: Rect,
    buf: &mut Buffer,
    state: &ListPaneState,
    pane_style: &super::ListPaneStyle,
) {
    if area.width == 0 || area.height == 0 || !pane_style.show_corner_indicators {
        return;
    }

    let indicator_fg = pane_style.indicator_fg;

    let top_right = (area.x + area.width - 1, area.y);
    let bottom_right = (area.x + area.width - 1, area.y + area.height - 1);

    // Helper: place an indicator with `… ` padding if it overwrites content.
    // Result: `content… ▼` — truncation ellipsis + space + indicator.
    //
    // Preserves each cell's bg (e.g., selection highlight). The `…` inherits
    // the overwritten content's fg color; the indicator uses the given fg.
    let place_indicator =
        |buf: &mut Buffer, pos: (u16, u16), symbol: &str, fg: ratatui::style::Color| {
            // Check if the indicator or the cell just before it has content.
            // If so, insert `… ` padding to avoid the indicator visually
            // merging with text (e.g., `count=3▶` → `count… ▶`).
            if area.width >= 3 && pos.0 >= area.x + 2 {
                let at_pos = buf[pos].symbol().to_string();
                let before_pos = buf[(pos.0 - 1, pos.1)].symbol().to_string();
                let has_adjacent_content = !at_pos.chars().all(char::is_whitespace)
                    || !before_pos.chars().all(char::is_whitespace);
                if has_adjacent_content {
                    let ellipsis_fg = buf[(pos.0 - 2, pos.1)].fg;
                    buf[(pos.0 - 2, pos.1)].set_symbol("…");
                    buf[(pos.0 - 2, pos.1)].fg = ellipsis_fg;
                    buf[(pos.0 - 1, pos.1)].set_symbol(" ");
                }
            }
            // Indicator: set symbol + fg, preserve bg
            buf[pos].set_symbol(symbol);
            buf[pos].fg = fg;
            buf[pos].modifier = ratatui::style::Modifier::empty();
        };

    // Top-right: ▲ when there's content above.
    if state.scroll_offset() > 0 {
        place_indicator(buf, top_right, "▲", indicator_fg);
    }

    // Bottom-right: ▶ in follow mode (distinct color), ▼ when more below.
    let total = state.total_height();
    let vp = area.height as usize;
    let at_bottom = total <= vp || state.scroll_offset() + vp >= total;

    if state.follow_mode {
        place_indicator(buf, bottom_right, "▶", pane_style.follow_indicator_fg);
    } else if !at_bottom {
        place_indicator(buf, bottom_right, "▼", indicator_fg);
    }
}

// ---------------------------------------------------------------------------
// Input bar rendering
// ---------------------------------------------------------------------------

/// Render the bottom bar: active input bar or accepted matcher status.
///
/// When the input bar is open: left-aligned editable `search: ` or `filter: ` + textarea.
/// When a matcher is accepted (bar closed): right-aligned dim status.
fn render_bottom_bar(
    area: Rect,
    buf: &mut Buffer,
    state: &mut ListPaneState,
    style: &ListPaneStyle,
) {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    if area.width == 0 || area.height == 0 {
        return;
    }

    // Background for the entire bar row.
    buf.set_style(area, Style::default().bg(style.input_bar_bg));

    if let Some(mode) = state.input_mode() {
        // Active input bar — left-aligned, editable.
        let label = match mode {
            super::state::InputBarMode::Search => "search: ",
            super::state::InputBarMode::Filter => "filter: ",
            super::state::InputBarMode::GotoLine => "go to: ",
            super::state::InputBarMode::Comment => "comment: ",
        };
        let label_style = Style::default()
            .fg(style.input_bar_prompt_fg)
            .bg(style.input_bar_bg);
        let label_line = Line::from(Span::styled(label, label_style));
        let label_w = label.len() as u16;
        buf.set_line_safe(area.x, area.y, &label_line, label_w);

        // Textarea fills the rest. Multi-line for comment mode.
        let ta_area = Rect {
            x: area.x + label_w,
            y: area.y,
            width: area.width.saturating_sub(label_w),
            height: area.height,
        };
        if ta_area.width > 0 {
            state.render_input_textarea(ta_area, buf);
        }
    } else if let Some(matcher) = state.matcher() {
        // Accepted matcher — right-aligned, dim.
        let mode_word = match matcher.mode {
            super::state::MatchMode::Filter => "filter",
            super::state::MatchMode::Search => "search",
        };
        let status = format!("[{}: {}]  ", mode_word, matcher.query());
        let status_w = status.len() as u16;
        let dim_style = Style::default()
            .fg(style.input_bar_text_fg)
            .bg(style.input_bar_bg)
            .add_modifier(Modifier::DIM);

        // Right-align.
        let x = area.x.saturating_add(area.width.saturating_sub(status_w));
        let status_line = Line::from(Span::styled(status, dim_style));
        buf.set_line_safe(x, area.y, &status_line, area.width);
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::TracingEntry;
    use crate::views::list_pane::layout::WrapMode;
    use crate::views::list_pane::{
        FilterMatcher, ListMatcher, ListPaneStyle, MatchMode, QueryKind,
    };
    use ratatui::style::Style;
    use ratatui::text::Line;

    /// A test item that renders its id as text.
    #[derive(Debug, Clone)]
    struct RenderTestItem {
        id: u64,
        height: u16,
        text: String,
    }

    impl RenderTestItem {
        fn new(id: u64, text: &str) -> Self {
            Self {
                id,
                height: 1,
                text: text.to_string(),
            }
        }

        #[allow(dead_code)]
        fn with_height(mut self, h: u16) -> Self {
            self.height = h;
            self
        }
    }

    impl ListItem for RenderTestItem {
        fn render(&self, area: Rect, buf: &mut Buffer, selected: bool, _focused: bool) {
            // Render text, with ">" prefix if selected.
            let prefix = if selected { ">" } else { " " };
            let text = format!("{}{}", prefix, self.text);
            let style = Style::default();
            for (i, ch) in text.chars().enumerate() {
                let x = area.x + i as u16;
                if x < area.x + area.width {
                    buf[(x, area.y)].set_char(ch).set_style(style);
                }
            }
        }

        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }

        fn stable_id(&self) -> u64 {
            self.id
        }

        fn search_text(&self) -> &str {
            &self.text
        }

        fn search_text_col_offset(&self) -> u16 {
            1 // prefix ">" or " "
        }
    }

    /// Helper: extract text from a buffer row.
    fn row_text(buf: &Buffer, y: u16, x_start: u16, width: u16) -> String {
        (x_start..x_start + width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn renders_basic_items() {
        let items = vec![
            RenderTestItem::new(0, "alpha"),
            RenderTestItem::new(1, "beta"),
            RenderTestItem::new(2, "gamma"),
        ];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // All three items visible. Item 0 is auto-selected (">").
        assert_eq!(row_text(&buf, 0, 0, 20), ">alpha");
        assert_eq!(row_text(&buf, 1, 0, 20), " beta");
        assert_eq!(row_text(&buf, 2, 0, 20), " gamma");
    }

    #[test]
    fn renders_with_selection() {
        let items = vec![
            RenderTestItem::new(0, "alpha"),
            RenderTestItem::new(1, "beta"),
            RenderTestItem::new(2, "gamma"),
        ];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        // Auto-selected item 0. One select_next → item 1.
        state.select_next(&items);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Item 1 should have ">" prefix, others have " " prefix.
        assert_eq!(row_text(&buf, 0, 0, 20), " alpha");
        assert_eq!(row_text(&buf, 1, 0, 20), ">beta");
        assert_eq!(row_text(&buf, 2, 0, 20), " gamma");
    }

    #[test]
    fn renders_scrolled_view() {
        let items: Vec<RenderTestItem> = (0..10)
            .map(|i| RenderTestItem::new(i, &format!("item-{i}")))
            .collect();
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        // Viewport of 3 rows, no scrollbar since we want to test scroll position.
        let area = Rect::new(0, 0, 20, 3);
        state.prepare_layout(&items, area.width, area.height);

        // Scroll down by 5.
        state.scroll_down(5);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Should show items 5, 6, 7 (with scrollbar taking 2 cols).
        let text_0 = row_text(&buf, 0, 0, 18);
        let text_1 = row_text(&buf, 1, 0, 18);
        let text_2 = row_text(&buf, 2, 0, 18);
        assert!(
            text_0.contains("item-5"),
            "row 0 should show item-5, got: {text_0}"
        );
        assert!(
            text_1.contains("item-6"),
            "row 1 should show item-6, got: {text_1}"
        );
        assert!(
            text_2.contains("item-7"),
            "row 2 should show item-7, got: {text_2}"
        );
    }

    #[test]
    fn renders_empty_list() {
        let items: Vec<RenderTestItem> = vec![];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Should not crash, all rows empty.
        assert_eq!(row_text(&buf, 0, 0, 20), "");
    }

    #[test]
    fn renders_with_filter() {
        let items = vec![
            RenderTestItem::new(0, "alpha"),
            RenderTestItem::new(1, "beta"),
            RenderTestItem::new(2, "alphabet"),
            RenderTestItem::new(3, "gamma"),
        ];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);

        // Filter to items containing "alph".
        state.set_filter(Some(FilterMatcher::substring("alph")));
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Should show "alpha" (auto-selected) and "alphabet".
        assert_eq!(row_text(&buf, 0, 0, 20), ">alpha");
        assert_eq!(row_text(&buf, 1, 0, 20), " alphabet");
        assert_eq!(row_text(&buf, 2, 0, 20), "");
    }

    #[test]
    fn truncation_ellipsis_appended_after_text() {
        // Item with desired_height > 1 in NoWrap mode → gets truncation "…".
        // Text "hello" (6 chars with prefix " ") in a 20-char-wide area,
        // so the "…" should be appended at position 6.
        let items = vec![
            RenderTestItem::new(0, "hello").with_height(3), // would be 3 lines tall
        ];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // ">hello…" — "…" appended after text since there's trailing space.
        let row = row_text(&buf, 0, 0, 20);
        assert!(
            row.contains("…"),
            "expected truncation ellipsis, got: {row}"
        );
        assert!(row.starts_with(">hello"), "expected '>hello', got: {row}");
    }

    #[test]
    fn truncation_ellipsis_replaces_last_char_at_full_width() {
        // Text fills the exact width → "…" replaces the last character.
        // Width 7, prefix " " = 1 char, so text area = 6 chars.
        // "abcdef" fills all 7 columns (1 prefix + 6 text).
        let items = vec![RenderTestItem::new(0, "abcdef").with_height(2)];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 7, 5);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // " abcde…" — last char 'f' replaced by "…".
        let row = row_text(&buf, 0, 0, 7);
        assert!(row.ends_with('…'), "expected trailing …, got: {row}");
        assert_eq!(row, ">abcde…");
    }

    #[test]
    fn no_truncation_ellipsis_for_short_items() {
        // Item with desired_height == 1 → no truncation, no ellipsis.
        let items = vec![RenderTestItem::new(0, "short")];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        let row = row_text(&buf, 0, 0, 20);
        assert!(
            !row.contains('…'),
            "no ellipsis expected for short items, got: {row}"
        );
        assert_eq!(row, ">short");
    }

    // -- Match highlight tests ------------------------------------------------

    #[test]
    fn highlight_match_inverts_correct_cells() {
        // Items: "alpha", "beta", "alphabet"
        // Search for "alph" → should invert fg/bg on match cells in items 0 and 2.
        let items = vec![
            RenderTestItem::new(0, "alpha"),
            RenderTestItem::new(1, "beta"),
            RenderTestItem::new(2, "alphabet"),
        ];

        // Render WITHOUT search to capture baseline colors.
        let area = Rect::new(0, 0, 20, 5);
        let mut state_base = ListPaneState::new(WrapMode::NoWrap, false);
        state_base.prepare_layout(&items, area.width, area.height);
        let mut buf_base = Buffer::empty(area);
        let pane_base = ListPane::new(&items);
        StatefulWidget::render(pane_base, area, &mut buf_base, &mut state_base);

        // Render WITH search.
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        state.set_matcher(Some(ListMatcher::new(
            "alph",
            QueryKind::Substring,
            MatchMode::Search,
        )));
        state.prepare_layout(&items, area.width, area.height);
        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Item 0: ">alpha" — "alph" is at columns 1..5 (after ">" prefix).
        // Match cells should have the REVERSED modifier set.
        for col in 1..5u16 {
            let cell = &buf[(col, 0)];
            assert!(
                cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                "col {col}: should have REVERSED modifier",
            );
        }
        // Column 5 ('a' of "alpha") should NOT be reversed.
        assert!(
            !buf[(5, 0)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "col 5 should not be reversed"
        );

        // Item 1: " beta" — no match, no REVERSED.
        for col in 0..5u16 {
            assert!(
                !buf[(col, 1)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED),
                "item 1 col {col} should not be reversed"
            );
        }

        // Item 2: " alphabet" — "alph" at columns 1..5.
        for col in 1..5u16 {
            assert!(
                buf[(col, 2)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED),
                "item 2 col {col}: should have REVERSED modifier"
            );
        }
    }

    #[test]
    fn selection_bg_and_highlight_inversion_both_applied() {
        // Selected item with a match: non-match cells get selection_bg,
        // match cells get REVERSED modifier (on top of selection_bg).
        let items = vec![RenderTestItem::new(0, "hello world")];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        state.set_matcher(Some(ListMatcher::new(
            "world",
            QueryKind::Substring,
            MatchMode::Search,
        )));
        let area = Rect::new(0, 0, 20, 5);
        state.prepare_layout(&items, area.width, area.height);

        let style = ListPaneStyle::default();
        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items).style(style);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Non-match cells should have selection_bg but NOT REVERSED.
        let sel_bg = style.selection_bg;
        assert_eq!(buf[(0, 0)].bg, sel_bg, "selection bg at col 0");
        assert_eq!(buf[(6, 0)].bg, sel_bg, "selection bg at col 6");
        assert!(
            !buf[(0, 0)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "non-match col 0 should not be reversed"
        );

        // Match cells ("world" at columns 7..12) should have REVERSED.
        for col in 7..12u16 {
            assert!(
                buf[(col, 0)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED),
                "match col {col} should have REVERSED modifier"
            );
        }
    }

    /// Regression: search highlight in Wrap mode should highlight the correct
    /// cells even when text wraps across multiple rows.
    ///
    /// Uses a realistic tracing line that wraps, with a search for "tool".
    #[test]
    fn highlight_match_wrap_mode_correct_positions() {
        // A long line that wraps at width 40. Contains "tool" near the end.
        let text = "abcdefghij klmnopqrst uvwxyz0123 tool_call foo bar baz qux";
        let items = vec![RenderTestItem::new(0, text)];

        let width = 40u16;
        let height = 5u16;
        let area = Rect::new(0, 0, width, height);

        // Render WITH search in Wrap mode.
        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.set_matcher(Some(ListMatcher::new(
            "tool",
            QueryKind::Substring,
            MatchMode::Search,
        )));
        state.prepare_layout(&items, area.width, area.height);
        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Find where "tool" appears visually in the buffer.
        // The match is at byte offset 32 in the plain text.
        let byte_pos = text.find("tool").unwrap();
        assert_eq!(byte_pos, 33);

        // Find which cells have REVERSED modifier (= highlighted).
        let mut reversed_cells: Vec<(u16, u16)> = Vec::new();
        for row in 0..height {
            for col in 0..width {
                if buf[(col, row)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    reversed_cells.push((col, row));
                }
            }
        }

        // The highlighted cells should spell "tool" — verify by checking
        // that the symbols at those positions form "tool".
        let highlighted_text: String = reversed_cells
            .iter()
            .map(|&(c, r)| buf[(c, r)].symbol().to_string())
            .collect();
        assert_eq!(
            highlighted_text, "tool",
            "Highlighted cells should spell 'tool', got '{highlighted_text}' \
             at positions {reversed_cells:?}"
        );
    }

    /// Regression: long synthetic tracing line at terminal width 159.
    /// Search for "tool" highlights wrong positions due to wrap mismatch.
    #[test]
    fn highlight_match_wrap_mode_real_tracing_line() {
        use ratatui::style::{Color, Modifier};
        use ratatui::text::{Line, Span};

        // Synthetic tracing line shaped like production logs (plain, ANSI-stripped).
        let plain = "2026-02-23T15:01:05.563125Z  INFO session.handle_prompt{session_id=019e0000-0000-7000-8000-000000000001 prompt_id=019e0000-0000-7000-8000-000000000011 prompt_preview=\"<user_query>\\nwhats the date, use bash command\\n</user_query>\"}:session.process_conversation_turn_with_recovery{req_id=019e0000-0000-7000-8000-000000000011 session_id=019e0000-0000-7000-8000-000000000001}:session.process_conversation_turn{session_id=019e0000-0000-7000-8000-000000000001}:tools.execute{tool_count=1}: echo_agent_runtime::session::acp_session: Model requesting tool: name='run_terminal_cmd', call_id='toolu_fake_01ABCDEFGHIJKLMNOPQRSTUV', arguments={\"command\": \"date\", \"description\": \"Get the current date\"}";

        // Styled version with multiple spans (simulating ansi-to-tui output).
        let styled = Line::from(vec![
            Span::styled(
                "2026-02-23T15:01:05.563125Z",
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled("INFO", Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled(
                "session.handle_prompt{",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "session_id",
                Style::default().add_modifier(Modifier::ITALIC),
            ),
            Span::raw("=019e0000-0000-7000-8000-000000000001 "),
            Span::styled("prompt_id", Style::default().add_modifier(Modifier::ITALIC)),
            Span::raw("=019e0000-0000-7000-8000-000000000011 "),
            Span::styled(
                "prompt_preview",
                Style::default().add_modifier(Modifier::ITALIC),
            ),
            Span::raw("=\"<user_query>\\nwhats the date, use bash command\\n</user_query>\""),
            Span::styled("}", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(":", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "session.process_conversation_turn_with_recovery{",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("req_id", Style::default().add_modifier(Modifier::ITALIC)),
            Span::raw("=019e0000-0000-7000-8000-000000000011 "),
            Span::styled(
                "session_id",
                Style::default().add_modifier(Modifier::ITALIC),
            ),
            Span::raw("=019e0000-0000-7000-8000-000000000001"),
            Span::styled("}", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(
                ":session.process_conversation_turn{session_id=019e0000-0000-7000-8000-000000000001}:tools.execute{tool_count=1}",
            ),
            Span::styled(":", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(
                "echo_agent_runtime::session::acp_session:",
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(
                " Model requesting tool: name='run_terminal_cmd', call_id='toolu_fake_01ABCDEFGHIJKLMNOPQRSTUV', arguments={\"command\": \"date\", \"description\": \"Get the current date\"}",
            ),
        ]);

        // Verify the flattened styled text matches the plain text.
        let flat: String = styled.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            flat, plain,
            "styled spans must flatten to same text as search_text"
        );

        #[derive(Debug, Clone)]
        struct TracingItem {
            plain: String,
            styled: Line<'static>,
        }
        impl super::super::ListItem for TracingItem {
            fn content(&self) -> &Line<'_> {
                &self.styled
            }
            fn stable_id(&self) -> u64 {
                0
            }
            fn search_text(&self) -> &str {
                &self.plain
            }
        }

        let item = TracingItem {
            plain: plain.to_string(),
            styled,
        };
        let items = vec![item];

        let width = 159u16;
        let height = 10u16;
        let area = Rect::new(0, 0, width, height);

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.set_matcher(Some(ListMatcher::new(
            "tool",
            QueryKind::Substring,
            MatchMode::Search,
        )));
        state.prepare_layout(&items, area.width, area.height);
        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Find all highlighted cells and check they spell "tool".
        let mut reversed_cells: Vec<(u16, u16)> = Vec::new();
        for row in 0..height {
            for col in 0..width {
                if buf[(col, row)].modifier.contains(Modifier::REVERSED) {
                    reversed_cells.push((col, row));
                }
            }
        }

        let highlighted_text: String = reversed_cells
            .iter()
            .map(|&(c, r)| buf[(c, r)].symbol().to_string())
            .collect();

        // "tool" appears multiple times in the text. Each occurrence should
        // highlight exactly "tool" (4 chars). Check that the highlighted text
        // is a concatenation of "tool" instances.
        let tool_count = plain.matches("tool").count();
        let expected = "tool".repeat(tool_count);
        assert_eq!(
            highlighted_text, expected,
            "Highlighted cells should spell '{}' ({} occurrences), got '{}' \
             at positions {:?}",
            expected, tool_count, highlighted_text, reversed_cells,
        );
    }

    /// Regression: search highlight in Wrap mode with multi-span styled content.
    ///
    /// Tracing entries have ANSI-parsed styled spans. The search_text() is
    /// plain (ANSI-stripped), but the content() has multiple styled spans.
    /// Wrap positions and highlight byte offsets must stay in sync.
    #[test]
    fn highlight_match_wrap_mode_styled_spans() {
        use ratatui::style::Color;
        use ratatui::text::{Line, Span};
        // Simulate a styled tracing line: "INFO " (green) + long message
        // containing "tool" after a wrap boundary.
        let prefix_part = "INFO ";
        let msg_part = "session.handle_prompt request_id=abc model_name=test: tool_execute command";
        let full_plain = format!("{prefix_part}{msg_part}");

        // Create item with two styled spans but search_text returning plain.
        #[derive(Debug, Clone)]
        struct StyledItem {
            plain: String,
            styled: Line<'static>,
        }
        impl super::super::ListItem for StyledItem {
            fn content(&self) -> &Line<'_> {
                &self.styled
            }
            fn stable_id(&self) -> u64 {
                0
            }
            fn search_text(&self) -> &str {
                &self.plain
            }
        }

        let styled = Line::from(vec![
            Span::styled(prefix_part.to_owned(), Style::default().fg(Color::Green)),
            Span::raw(msg_part.to_owned()),
        ]);
        let item = StyledItem {
            plain: full_plain.clone(),
            styled,
        };
        let items = vec![item];

        let width = 40u16;
        let height = 5u16;
        let area = Rect::new(0, 0, width, height);

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.set_matcher(Some(ListMatcher::new(
            "tool",
            QueryKind::Substring,
            MatchMode::Search,
        )));
        state.prepare_layout(&items, area.width, area.height);
        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        // Find highlighted cells.
        let mut reversed_cells: Vec<(u16, u16)> = Vec::new();
        for row in 0..height {
            for col in 0..width {
                if buf[(col, row)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    reversed_cells.push((col, row));
                }
            }
        }

        let highlighted_text: String = reversed_cells
            .iter()
            .map(|&(c, r)| buf[(c, r)].symbol().to_string())
            .collect();
        assert_eq!(
            highlighted_text, "tool",
            "Highlighted cells should spell 'tool', got '{highlighted_text}' \
             at positions {reversed_cells:?}"
        );
    }

    // =========================================================================
    // Long line wrapping bug regression tests
    // =========================================================================

    /// A content-based test item (uses the framework's wrapping).
    #[derive(Debug)]
    struct ContentTestItem {
        id: u64,
        line: Line<'static>,
    }

    impl ContentTestItem {
        fn new(id: u64, text: &str) -> Self {
            Self {
                id,
                line: Line::from(text.to_string()),
            }
        }
    }

    impl ListItem for ContentTestItem {
        fn content(&self) -> &Line<'_> {
            &self.line
        }

        fn stable_id(&self) -> u64 {
            self.id
        }

        fn search_text(&self) -> &str {
            // Flatten spans to get searchable text
            self.line
                .spans
                .first()
                .map(|s| s.content.as_ref())
                .unwrap_or("")
        }
    }

    /// Synthetic long tracing line (800+ chars) for wrap regression tests.
    const LONG_LINE: &str = r#"2026-03-06T20:17:47.790351Z  INFO session.handle_prompt{session_id=019e0000-0000-7000-8000-000000000002 prompt_id=019e0000-0000-7000-8000-000000000012 prompt_preview="<user_query>\ncheck current weather in 10 ways\n</user_query>"}:session.process_conversation_turn_with_recovery{req_id=019e0000-0000-7000-8000-000000000012 session_id=019e0000-0000-7000-8000-000000000002}:session.process_conversation_turn{session_id=019e0000-0000-7000-8000-000000000002}:tools.execute{tool_count=10}: echo_agent_runtime::session::acp_session: Model requesting tool: name='run_terminal_cmd', call_id='toolu_fake_01WXYZABCDEFGHIJKLMNOPQR', arguments={"command": "curl -s \"v2.wttr.in/?0\" 2>/dev/null", "description": "Way 6: v2.wttr.in fancy graphical view", "timeout": 15000}"#;

    /// Helper: collect all non-space characters from buffer as a String.
    fn collect_rendered(buf: &Buffer, width: u16, height: u16) -> String {
        let mut s = String::new();
        for y in 0..height {
            for x in 0..width {
                let sym = buf[(x, y)].symbol();
                if sym != " " {
                    s.push_str(sym);
                }
            }
        }
        s
    }

    // =========================================================================
    // Long line wrapping regression tests
    // =========================================================================

    #[test]
    fn long_line_desired_height_is_accurate() {
        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);
        let height = item.desired_height(width);

        // ~800 chars at width 112 should need ~7-8 lines
        assert!(
            height >= 7,
            "Long line should need at least 7 rows at width {}",
            width
        );
    }

    #[test]
    fn long_line_renders_all_wrapped_rows() {
        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);
        let items = [item];
        let height = items[0].desired_height(width);
        let viewport_height = height + 5;

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, width, viewport_height);

        let area = Rect::new(0, 0, width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let non_empty_rows = (0..viewport_height)
            .filter(|&y| (0..width).any(|x| buf[(x, y)].symbol().trim() != ""))
            .count() as u16;

        assert_eq!(
            non_empty_rows, height,
            "Rendered rows should match desired_height"
        );
    }

    #[test]
    fn long_line_content_is_complete() {
        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);
        let items = [item];
        let height = items[0].desired_height(width);
        let viewport_height = height + 5;

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, width, viewport_height);

        let area = Rect::new(0, 0, width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, width, viewport_height);
        assert!(
            rendered.contains("timeout"),
            "Rendered should contain 'timeout'"
        );
        assert!(
            rendered.contains("15000"),
            "Rendered should contain '15000'"
        );
    }

    #[test]
    fn escaped_newlines_not_split() {
        // \n in JSON strings should NOT be interpreted as actual newlines
        let width: u16 = 112;
        let line = r#"INFO prompt_preview="<user_query>\ncheck weather\n</user_query>" arguments={"timeout": 15000}"#;

        let item = ContentTestItem::new(0, line);
        let items = [item];

        // Content should equal original text
        let flattened: String = items[0]
            .content()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(flattened, line);

        // Render and verify content is complete
        let height = items[0].desired_height(width);
        let viewport_height = height + 5;
        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, width, viewport_height);

        let area = Rect::new(0, 0, width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, width, viewport_height);
        assert!(
            rendered.contains("15000"),
            "Rendered should contain '15000'"
        );
    }

    #[test]
    fn tracing_entry_renders_complete_content() {
        // Test TracingEntry with ANSI codes renders completely.
        let width: u16 = 112;

        let ansi_line = format!(
            "\x1b[2m2026-03-06T20:17:47.790351Z\x1b[0m \x1b[32m INFO\x1b[0m {}",
            r#"Model requesting tool: name='run_terminal_cmd', arguments={"command": "curl", "timeout": 15000}"#
        );

        let entry = TracingEntry::new(0, &ansi_line);
        let items = [entry];

        // Verify plain text contains the complete content
        assert!(
            items[0].search_text().contains("timeout"),
            "Plain text should contain 'timeout'"
        );

        // Render and check all content is present
        let height = items[0].desired_height(width);
        let viewport_height = height + 5;
        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        let area = Rect::new(0, 0, width, viewport_height);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        let pane = ListPane::new(&items);
        StatefulWidget::render(pane, area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, width, viewport_height);
        assert!(
            rendered.contains("timeout"),
            "Rendered should contain 'timeout'"
        );
        assert!(
            rendered.contains("15000"),
            "Rendered should contain '15000'"
        );
    }

    #[test]
    fn long_line_constrained_viewport_clips_correctly() {
        // Constrained viewport should clip item, not corrupt layout.
        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);
        let items = [item];

        let full_height = items[0].desired_height(width);
        let viewport_height = (full_height / 2).max(2);

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        let area = Rect::new(0, 0, width, viewport_height);
        state.prepare_layout(&items, area.width, area.height);

        // Layout height should match desired height (not be clipped)
        assert_eq!(
            state.layout().item_height(0),
            full_height,
            "Layout cache height should match desired height"
        );

        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        // Count rendered rows - should fill viewport
        let non_empty_rows = (0..viewport_height)
            .filter(|&y| (0..width).any(|x| buf[(x, y)].symbol().trim() != ""))
            .count() as u16;

        assert_eq!(
            non_empty_rows, viewport_height,
            "Should fill viewport when item is taller than viewport"
        );
    }

    #[test]
    fn multiple_long_items_layout_integrity() {
        // Test layout cache integrity with multiple long items.
        let width: u16 = 112;

        let items = [
            ContentTestItem::new(0, &"A".repeat(300)),
            ContentTestItem::new(1, LONG_LINE),
            ContentTestItem::new(2, &"C".repeat(400)),
        ];

        let heights: Vec<u16> = items.iter().map(|i| i.desired_height(width)).collect();
        let total_desired: u16 = heights.iter().sum();

        let viewport_height = total_desired + 5;
        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, width, viewport_height);

        // Verify layout cache heights
        for (i, &h) in heights.iter().enumerate() {
            assert_eq!(
                state.layout().item_height(i),
                h,
                "Item {} height mismatch",
                i
            );
        }
        assert_eq!(state.layout().total_height(), total_desired as usize);

        // Render and verify content
        let area = Rect::new(0, 0, width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, width, viewport_height);
        assert!(rendered.starts_with("A"), "Should start with item 0");
        assert!(
            rendered.contains("15000"),
            "Item 1 should render completely"
        );
        assert!(rendered.ends_with("C"), "Should end with item 2");
    }

    #[test]
    fn scrolled_long_item_shows_end_content() {
        // Scrolled view should show end of long item.
        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);
        let items = [item];

        let full_height = items[0].desired_height(width);
        let viewport_height = 3u16;

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, width, viewport_height);

        // Scroll to show the last rows
        let scroll_to = (full_height as usize).saturating_sub(viewport_height as usize);
        state.scroll_down(scroll_to);

        let area = Rect::new(0, 0, width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, width, viewport_height);
        assert!(
            rendered.contains("timeout") || rendered.contains("15000"),
            "Scrolled view should show end of content"
        );
    }

    #[test]
    fn wrap_line_count_matches_desired_height() {
        // word_wrap_line output count must match desired_height.
        use crate::render::wrapping::word_wrap_line;

        let width: u16 = 112;
        let item = ContentTestItem::new(0, LONG_LINE);

        let desired_h = item.desired_height(width);
        let wrapped = word_wrap_line(item.content(), width as usize);

        assert_eq!(
            wrapped.len() as u16,
            desired_h,
            "word_wrap_line count should match desired_height"
        );

        // Verify all content is present
        let total_text: String = wrapped
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        assert!(
            total_text.contains("timeout"),
            "Wrapped output should contain 'timeout'"
        );
    }

    // =========================================================================
    // Scrollbar width mismatch bug (regression tests for the fix)
    // =========================================================================

    #[test]
    fn scrollbar_width_mismatch_bug_repro() {
        // BUG REPRODUCTION: Documents the scrollbar width mismatch issue.
        //
        // Root cause (without fix):
        // 1. prepare_layout() computes heights at width W (114)
        // 2. Scrollbar reduces render width to W-2 (112)
        // 3. Item needs MORE lines at narrower width
        // 4. Layout allocates fewer rows than needed → truncation
        //
        // This test verifies the bug EXISTS at the item level.
        // The fix in prepare_layout prevents this by computing at narrow width.
        use crate::render::wrapping::word_wrap_line;

        let layout_width: u16 = 114;
        let render_width: u16 = 112;

        let item = ContentTestItem::new(0, LONG_LINE);
        let height_at_layout = item.desired_height(layout_width);
        let height_at_render = item.desired_height(render_width);

        // Critical bug condition: narrower width needs MORE lines
        assert!(
            height_at_render > height_at_layout,
            "Bug trigger: narrower width needs more lines ({} > {})",
            height_at_render,
            height_at_layout
        );

        // Without fix: allocated = 7, needed = 8 → 1 line truncated
        let wrapped = word_wrap_line(item.content(), render_width as usize);
        let lines_truncated = (wrapped.len() as u16).saturating_sub(height_at_layout);

        assert!(
            lines_truncated > 0,
            "Bug documented: {} lines would be truncated",
            lines_truncated
        );
    }

    #[test]
    fn scrollbar_width_fix_verified() {
        // Verifies the prepare_layout fix works end-to-end.
        //
        // The fix (Option B): compute at narrow width when scrollbar is needed.
        // Phase 1: vis_count > viewport → scrollbar definite → width-2
        // Phase 2: total_height > viewport → fallback recompute at width-2
        let full_width: u16 = 114;
        let narrow_width: u16 = 112;

        // 2 items guarantee scrollbar (Phase 1)
        let items: Vec<ContentTestItem> = vec![
            ContentTestItem::new(0, LONG_LINE),
            ContentTestItem::new(1, LONG_LINE),
        ];

        let height_narrow = items[0].desired_height(narrow_width);
        let viewport_height: u16 = 10;

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, full_width, viewport_height);

        // Fix should compute at narrow width
        assert_eq!(
            state.layout().item_height(0),
            height_narrow,
            "Fix should compute height at narrow width"
        );

        // Render and verify content is complete
        let area = Rect::new(0, 0, full_width, viewport_height);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        let rendered = collect_rendered(&buf, full_width, viewport_height);
        assert!(
            rendered.contains("15000"),
            "Fix: '15000' should be rendered (was truncated before)"
        );
        assert!(
            rendered.contains("timeout"),
            "Fix: 'timeout' should be rendered"
        );
    }

    #[test]
    fn scrollbar_fix_phase1_many_items() {
        // Phase 1: vis_count > viewport → scrollbar definite → compute at width-2
        let full_width: u16 = 114;
        let viewport_height: u16 = 5;

        let items: Vec<ContentTestItem> = (0..10)
            .map(|i| ContentTestItem::new(i, LONG_LINE))
            .collect();

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, full_width, viewport_height);

        let narrow_height = items[0].desired_height(full_width - 2);
        for i in 0..items.len() {
            assert_eq!(
                state.layout().item_height(i),
                narrow_height,
                "Item {} should have narrow-width height",
                i
            );
        }
    }

    #[test]
    fn scrollbar_fix_phase2_few_heavy_items() {
        // Phase 2: vis_count <= viewport but total_height > viewport
        // → fallback recompute at width-2
        let full_width: u16 = 114;
        let narrow_width: u16 = 112;

        let items = [ContentTestItem::new(0, LONG_LINE)];
        let height_narrow = items[0].desired_height(narrow_width);

        // Viewport = 6 < height at full width (7) → scrollbar needed
        // But vis_count (1) <= viewport (6) → Phase 1 skips, Phase 2 catches it
        let viewport_height: u16 = 6;

        let mut state = ListPaneState::new(WrapMode::Wrap, false);
        state.prepare_layout(&items, full_width, viewport_height);

        assert_eq!(
            state.layout().item_height(0),
            height_narrow,
            "Phase 2 should recompute at narrow width"
        );
    }

    /// Filter + item shrink must refilter vis_map; paint must not OOB.
    #[test]
    fn render_does_not_panic_when_filter_active_and_items_shrink() {
        let mut items = vec![
            RenderTestItem::new(0, "row-0"),
            RenderTestItem::new(1, "row-1"),
            RenderTestItem::new(2, "zzz"), // filtered out
            RenderTestItem::new(3, "row-3"),
            RenderTestItem::new(4, "row-4"),
        ];
        let mut state = ListPaneState::new(WrapMode::NoWrap, false);
        let area = Rect::new(0, 0, 20, 10);

        state.set_filter(Some(FilterMatcher::substring("row")));
        state.prepare_layout(&items, area.width, area.height);
        assert_eq!(state.visible_count(), 4);
        assert_eq!(state.selected_id(), Some(0));

        items.truncate(2);
        state.prepare_layout(&items, area.width, area.height);

        let mut buf = Buffer::empty(area);
        StatefulWidget::render(ListPane::new(&items), area, &mut buf, &mut state);

        assert_eq!(state.visible_count(), 2);
        assert_eq!(row_text(&buf, 0, 0, 20), ">row-0");
        assert_eq!(row_text(&buf, 1, 0, 20), " row-1");
    }
}
