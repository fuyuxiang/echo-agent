//! Dropdown renderer for shell command completion suggestions.
//!
//! Mirrors `slash_dropdown.rs` layout: aligned label column, description,
//! selection highlight, mouse hover, and scrollbar when items exceed
//! `MAX_VISIBLE_ROWS`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::render::SafeBuf;
use crate::render::line_utils::truncate_str;
use crate::render::scrollbar::render_scrollbar_styled;
use crate::theme::Theme;
use crate::views::suggestion_controller::{CompletionDropdownState, CompletionItemParsed};

/// Maximum visible rows in the completion dropdown.
pub const MAX_VISIBLE_ROWS: u16 = 6;

/// Hard cap on label column width.
const LABEL_CAP: usize = 40;

/// Gap between label and description columns.
const LABEL_DESC_GAP: usize = 2;

/// Prefix width (`"❯ "` or `"  "`).
const PREFIX_W: usize = 2;

/// Height needed for the dropdown (separator + items), or 0 when hidden.
pub fn dropdown_height(state: &CompletionDropdownState) -> u16 {
    if !state.open || state.items.is_empty() {
        return 0;
    }
    let item_rows = (state.items.len() as u16).min(MAX_VISIBLE_ROWS);
    1 + item_rows // separator + items
}

/// Compute the scroll offset so the selected row stays centred.
pub fn scroll_offset(state: &CompletionDropdownState) -> usize {
    let total = state.items.len();
    let visible = MAX_VISIBLE_ROWS as usize;
    let selected = state.selected.min(total.saturating_sub(1));
    if total <= visible || selected < visible / 2 {
        0
    } else if selected + visible / 2 >= total {
        total.saturating_sub(visible)
    } else {
        selected.saturating_sub(visible / 2)
    }
}

fn compute_label_column_w(items: &[CompletionItemParsed], content_w: usize) -> usize {
    let budget = (content_w * 3 / 5).min(LABEL_CAP);
    let max_w = items
        .iter()
        .map(|r| r.display.width())
        .filter(|&w| w <= LABEL_CAP)
        .max()
        .unwrap_or(0);
    max_w.min(budget)
}

/// Render completion dropdown items into `area` (no borders — caller draws chrome).
pub fn render_dropdown(
    buf: &mut Buffer,
    area: Rect,
    state: &CompletionDropdownState,
    theme: &Theme,
) {
    if area.height == 0 || area.width < 4 || !state.open {
        return;
    }

    let items = &state.items;
    let selected = state.selected.min(items.len().saturating_sub(1));
    let hovered = state.hovered;

    let content_w = area.width as usize;
    let visible_rows = area.height as usize;
    let needs_scrollbar = items.len() > visible_rows;
    let row_w = if needs_scrollbar {
        content_w.saturating_sub(2)
    } else {
        content_w
    };

    let label_col_w = compute_label_column_w(items, row_w.saturating_sub(PREFIX_W));

    let scroll = scroll_offset(state);

    for vis_row in 0..visible_rows {
        let item_idx = scroll + vis_row;
        if item_idx >= items.len() {
            break;
        }
        let item = &items[item_idx];
        let y = area.y + vis_row as u16;
        let is_selected = item_idx == selected;
        let is_hovered = hovered == Some(item_idx) && !is_selected;

        let row_bg = match crate::views::modal_window::embedded_row_style(theme, is_selected) {
            Some(e) => e.bg,
            None if is_selected => theme.bg_visual,
            None if is_hovered => theme.bg_hover,
            None => theme.bg_light,
        };

        let line = build_item_line(item, is_selected, label_col_w, row_w, row_bg, theme);

        // Skip rows that fall outside the buffer (resize race).
        if y < buf.area.y || y >= buf.area.bottom() || area.x >= buf.area.right() {
            continue;
        }
        let clamped_w = row_w.min(buf.area.right().saturating_sub(area.x) as usize) as u16;
        let row_rect = Rect {
            x: area.x,
            y,
            width: clamped_w,
            height: 1,
        };
        buf.set_style(row_rect, Style::default().bg(row_bg));
        buf.set_line_safe(area.x, y, &line, row_w as u16);
    }

    if needs_scrollbar {
        let sb_x = area.x + area.width.saturating_sub(1);
        let sb_y = area.y.max(buf.area.y);
        let sb_bottom = (area.y.saturating_add(area.height)).min(buf.area.bottom());
        if sb_x < buf.area.right() && sb_bottom > sb_y {
            let sb_area = Rect {
                x: sb_x,
                y: sb_y,
                width: 1,
                height: sb_bottom - sb_y,
            };
            let track = Style::default().bg(theme.bg_dark);
            let thumb = Style::default().fg(theme.gray_dim).bg(theme.bg_dark);
            render_scrollbar_styled(
                buf,
                Some(sb_area),
                items.len() as u16,
                sb_area.height,
                scroll as u16,
                track,
                thumb,
            );
        }
    }
}

fn build_item_line(
    item: &CompletionItemParsed,
    is_selected: bool,
    label_col_w: usize,
    total_w: usize,
    row_bg: ratatui::style::Color,
    theme: &Theme,
) -> Line<'static> {
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let embed = crate::views::modal_window::embedded_row_style(theme, is_selected);
    let primary_fg = embed.map_or(theme.text_primary, |e| e.fg(theme.text_primary));
    let desc_fg = embed.map_or(theme.gray, |e| e.fg(theme.gray));
    let normal = Style::default()
        .fg(primary_fg)
        .bg(row_bg)
        .add_modifier(bold);
    let desc_style = Style::default().fg(desc_fg).bg(row_bg);
    let bg_style = Style::default().bg(row_bg);

    let prefix = if is_selected {
        crate::glyphs::prompt_arrow()
    } else {
        "  "
    };
    let prefix_span = Span::styled(
        prefix.to_string(),
        if is_selected { normal } else { bg_style },
    );

    let label = truncate_str(&item.display, label_col_w);
    let label_w = label.width();
    let padding = label_col_w.saturating_sub(label_w);

    let label_span = Span::styled(label, normal);

    let desc_indent = PREFIX_W + label_col_w + LABEL_DESC_GAP;
    let desc_w = total_w.saturating_sub(desc_indent).max(1);
    let desc = truncate_str(&item.description, desc_w);

    let mut spans = vec![prefix_span, label_span];
    if padding > 0 {
        spans.push(Span::styled(" ".repeat(padding), bg_style));
    }
    if !desc.is_empty() {
        spans.push(Span::styled(" ".to_string(), bg_style));
        spans.push(Span::styled(desc, desc_style));
    }

    Line::from(spans).style(bg_style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::suggestion_controller::SuggestionSource;

    fn make_item(display: &str, desc: &str, insert: &str) -> CompletionItemParsed {
        CompletionItemParsed {
            display: display.into(),
            description: desc.into(),
            insert_text: insert.into(),
            source: SuggestionSource::History,
            priority: 0,
            replace_range: None,
            token_text: None,
            truncated: false,
        }
    }

    #[test]
    fn height_zero_when_closed() {
        let state = CompletionDropdownState::default();
        assert_eq!(dropdown_height(&state), 0);
    }

    /// Items area past buffer bottom must not panic on resize races.
    #[test]
    fn render_dropdown_past_buffer_bottom_does_not_panic() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let theme = Theme::current();
        let items: Vec<CompletionItemParsed> = (0..12)
            .map(|i| make_item(&format!("item{i}"), "desc", &format!("item{i}")))
            .collect();
        let state = CompletionDropdownState {
            open: true,
            items,
            selected: 0,
            ..Default::default()
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        let area = Rect::new(2, 8, 76, 8);
        render_dropdown(&mut buf, area, &state, &theme);
    }

    #[test]
    fn height_zero_when_empty() {
        let state = CompletionDropdownState {
            open: true,
            ..Default::default()
        };
        assert_eq!(dropdown_height(&state), 0);
    }

    #[test]
    fn height_with_items() {
        let state = CompletionDropdownState {
            open: true,
            items: vec![
                make_item("ls", "list", "ls"),
                make_item("cd", "change dir", "cd"),
            ],
            ..Default::default()
        };
        assert_eq!(dropdown_height(&state), 3); // 1 separator + 2 items
    }

    #[test]
    fn height_capped_at_max() {
        let items: Vec<_> = (0..20)
            .map(|i| make_item(&format!("cmd{i}"), "", &format!("cmd{i}")))
            .collect();
        let state = CompletionDropdownState {
            open: true,
            items,
            ..Default::default()
        };
        assert_eq!(dropdown_height(&state), 1 + MAX_VISIBLE_ROWS);
    }

    #[test]
    fn move_selection_wraps() {
        let mut state = CompletionDropdownState {
            open: true,
            items: vec![
                make_item("a", "", "a"),
                make_item("b", "", "b"),
                make_item("c", "", "c"),
            ],
            selected: 0,
            ..Default::default()
        };
        state.move_selection(-1);
        assert_eq!(state.selected, 2);
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn scroll_selection_clamps_at_edges() {
        let mut state = CompletionDropdownState {
            open: true,
            items: vec![
                make_item("a", "", "a"),
                make_item("b", "", "b"),
                make_item("c", "", "c"),
            ],
            selected: 0,
            ..Default::default()
        };
        // Scrolling up at the first item stays put (no wrap).
        state.scroll_selection(-1);
        assert_eq!(state.selected, 0);
        state.scroll_selection(1);
        assert_eq!(state.selected, 1);
        // Scrolling down at the last item stays put (no wrap).
        state.scroll_selection(1);
        assert_eq!(state.selected, 2);
        state.scroll_selection(1);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn accept_returns_item_and_closes() {
        let mut state = CompletionDropdownState {
            open: true,
            items: vec![make_item("ls -la", "list all", "ls -la /tmp")],
            selected: 0,
            ..Default::default()
        };
        let item = state.accept().expect("selected item accepted");
        assert_eq!(item.insert_text, "ls -la /tmp");
        assert!(!state.open);
    }

    #[test]
    fn accept_without_items_returns_none() {
        let mut state = CompletionDropdownState::default();
        assert!(state.accept().is_none());
    }

    /// `accept` is independent of the `open` render flag: the
    /// single-candidate insta-accept consumes an item that was never shown.
    #[test]
    fn accept_works_on_closed_dropdown_with_items() {
        let mut state = CompletionDropdownState {
            open: false,
            items: vec![make_item("ls", "", "ls -la")],
            ..Default::default()
        };
        let item = state.accept().expect("item accepted while closed");
        assert_eq!(item.insert_text, "ls -la");
    }

    #[test]
    fn scroll_offset_no_scroll_needed() {
        let state = CompletionDropdownState {
            open: true,
            items: vec![make_item("a", "", "a")],
            selected: 0,
            ..Default::default()
        };
        assert_eq!(scroll_offset(&state), 0);
    }

    #[test]
    fn scroll_offset_centres_selected() {
        let items: Vec<_> = (0..20)
            .map(|i| make_item(&format!("c{i}"), "", &format!("c{i}")))
            .collect();
        let state = CompletionDropdownState {
            open: true,
            items,
            selected: 10,
            ..Default::default()
        };
        let offset = scroll_offset(&state);
        let visible = MAX_VISIBLE_ROWS as usize;
        assert!(offset <= 10);
        assert!(offset + visible > 10);
    }

    #[test]
    fn close_resets_state() {
        let mut state = CompletionDropdownState {
            open: true,
            items: vec![make_item("a", "", "a")],
            selected: 0,
            hovered: Some(0),
            generation: 5,
            request_text: "a".into(),
            request_cursor: 1,
        };
        state.close();
        assert!(!state.open);
        assert_eq!(state.selected, 0);
        assert!(state.hovered.is_none());
        assert_eq!(state.generation, 5); // generation preserved
        assert!(state.items.is_empty());
        // Anchor left in place (inert without items); the next landing
        // overwrites it atomically with the new items.
        assert_eq!(state.request_text, "a");
        assert_eq!(state.request_cursor, 1);
    }
}
