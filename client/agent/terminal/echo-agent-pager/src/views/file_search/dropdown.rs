//! Dropdown list renderer for @-completion results.
//!
//! Renders fuzzy match results as a scrollable list with:
//! - Selection highlight (background color on selected row)
//! - Fuzzy match character highlighting (accent color on matched chars)
//! - Scrollbar when results exceed visible height
//! - Truncation with `…` for long paths
//! - Result count hint (e.g., "12/345") in the separator line

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use echo_agent_workspace::file_system::FuzzyMatchResult;

use crate::render::scrollbar::render_scrollbar_styled;
use crate::theme::Theme;

use super::context::normalize_display_path;
use super::state::FileSearchState;

/// Maximum number of visible rows in the dropdown (excluding separator).
pub const MAX_DROPDOWN_ROWS: u16 = 8;

/// Render the file search dropdown items into the given area.
///
/// This renders ONLY the result rows (no borders or separators).
/// Panel chrome (clear, borders, count hint) is handled by the caller
/// (AgentView). The `area` covers just the item rows.
pub fn render_dropdown(buf: &mut Buffer, area: Rect, file_search: &FileSearchState, theme: &Theme) {
    if area.height == 0 || area.width < 4 || !file_search.is_visible() {
        return;
    }

    let results = file_search.results();
    let topk = &results.topk;
    let selected = file_search.selected();
    let scroll = file_search.scroll_offset();
    let dir_mode = file_search.is_dir_mode();

    // Reserve 2 columns on the right for scrollbar (gap + track).
    let needs_scrollbar = topk.len() > area.height as usize;
    let content_width = if needs_scrollbar {
        area.width.saturating_sub(2)
    } else {
        area.width
    };

    let visible_rows = area.height as usize;

    let hovered = file_search.hovered();
    let hover_bg = theme.bg_hover;

    for row in 0..visible_rows {
        let idx = scroll + row;
        if idx >= topk.len() {
            break;
        }

        let item = &topk[idx];
        let y = area.y + row as u16;
        let is_selected = idx == selected;
        let is_hovered = hovered == Some(idx) && !is_selected;

        render_fuzzy_item(
            buf,
            area.x,
            y,
            content_width,
            item,
            is_selected,
            is_hovered,
            hover_bg,
            dir_mode,
            theme,
        );
    }

    // ── Scrollbar ───────────────────────────────────────────────────────

    if needs_scrollbar {
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let track_style = Style::default().bg(theme.bg_dark);
        let thumb_style = Style::default().fg(theme.gray_dim).bg(theme.bg_dark);
        render_scrollbar_styled(
            buf,
            Some(scrollbar_area),
            topk.len() as u16,
            area.height,
            scroll as u16,
            track_style,
            thumb_style,
        );
    }
}

/// Desired height for the dropdown (separator + min(results, max_rows)).
pub fn dropdown_height(file_search: &FileSearchState, max_rows: u16) -> u16 {
    if !file_search.is_visible() {
        return 0;
    }
    let result_rows = (file_search.result_count() as u16).min(max_rows);
    1 + result_rows // separator + results
}

/// Non-selected prefix — same width as the arrow, just spaces.
const ITEM_PREFIX: &str = "  ";
const PREFIX_WIDTH: u16 = crate::glyphs::PROMPT_ARROW_WIDTH;

/// Render a single fuzzy match item with character-level match highlighting.
#[allow(clippy::too_many_arguments)]
fn render_fuzzy_item(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    item: &FuzzyMatchResult,
    is_selected: bool,
    is_hovered: bool,
    hover_bg: ratatui::style::Color,
    dir_mode: bool,
    theme: &Theme,
) {
    if width < PREFIX_WIDTH + 1 {
        return;
    }

    let path_str = item.path.to_string();
    let path = normalize_display_path(&path_str);

    let embed = crate::views::modal_window::embedded_row_style(theme, is_selected);
    let row_bg = match embed {
        Some(e) => e.bg,
        None if is_selected => theme.bg_visual,
        None if is_hovered => hover_bg,
        None => theme.bg_light,
    };
    let text_fg = embed.map_or(theme.text_primary, |e| e.fg(theme.text_primary));
    let bold = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    // Fill the row with background.
    for col in x..x + width {
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_char(' ');
            cell.set_style(Style::default().bg(row_bg));
        }
    }

    // Arrow on the selected row, blank gutter on the rest.
    let prefix = if is_selected {
        crate::glyphs::prompt_arrow()
    } else {
        ITEM_PREFIX
    };
    let prefix_style = Style::default().fg(text_fg).bg(row_bg).add_modifier(bold);
    for (i, ch) in prefix.chars().enumerate() {
        let px = x + i as u16;
        if px < x + width
            && let Some(cell) = buf.cell_mut((px, y))
        {
            cell.set_char(ch);
            cell.set_style(if is_selected {
                prefix_style
            } else {
                Style::default().bg(row_bg)
            });
        }
    }

    // Styles: primary FG for text (not dimmed), BLUE for match chars.
    // Selected rows get bold via the modifier.
    let match_style = Style::default()
        .fg(embed.map_or(theme.fuzzy_accent, |e| e.fg(theme.fuzzy_accent)))
        .bg(row_bg)
        .add_modifier(bold);
    let normal_style = Style::default().fg(text_fg).bg(row_bg).add_modifier(bold);

    // Render path characters after prefix, with match highlighting.
    let mut indices = &item.indices[..];
    let mut col = x + PREFIX_WIDTH;
    let max_col = x + width;

    for (char_idx, (byte_idx, ch)) in path.char_indices().enumerate() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if col + ch_width > max_col {
            // Truncation: replace last visible char with '…'
            if col > x + PREFIX_WIDTH
                && let Some(cell) = buf.cell_mut((col.saturating_sub(1), y))
            {
                cell.set_char('…');
            }
            break;
        }

        let is_match = indices.first() == Some(&(char_idx as u32));
        if is_match {
            indices = &indices[1..];
        }

        let style = if is_match { match_style } else { normal_style };

        // Write the character.
        let ch_str = &path[byte_idx..byte_idx + ch.len_utf8()];
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_symbol(ch_str);
            cell.set_style(style);
        }
        // For wide chars, fill continuation cell.
        if ch_width > 1 {
            for w in 1..ch_width {
                if let Some(cell) = buf.cell_mut((col + w, y)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }
        }
        col += ch_width;
    }

    // In dir mode, append '/' after the path.
    if dir_mode
        && col < max_col
        && let Some(cell) = buf.cell_mut((col, y))
    {
        cell.set_char('/');
        cell.set_style(normal_style);
    }
}
