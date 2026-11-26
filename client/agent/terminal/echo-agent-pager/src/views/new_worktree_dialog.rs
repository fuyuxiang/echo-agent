//! Popup dialog for creating a new worktree with an optional label.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::app::app_view::NewWorktreeDialogState;
use crate::theme::Theme;

/// Minimum dialog width (fits title + empty input + hints comfortably).
const MIN_DIALOG_WIDTH: u16 = 50;
const DIALOG_HEIGHT: u16 = 5;
/// Left/right padding inside the border (`inner_x = dialog.x + 2`).
const INNER_PAD: u16 = 4;
const LABEL_PREFIX: &str = "Name (optional): ";

/// Render the new-worktree popup dialog centered on screen.
///
/// The dialog grows with the typed label up to the available width, then
/// scrolls the input viewport to keep the live cursor visible.
pub fn render_new_worktree_dialog(area: Rect, buf: &mut Buffer, state: &NewWorktreeDialogState) {
    let theme = Theme::current();

    let dialog_width = dialog_width_for(area.width, state.label());

    if area.height < DIALOG_HEIGHT || area.width < 20 {
        // Too small to render — draw a minimal "resize" hint so the user
        // knows the dialog is still active and can press Esc to dismiss.
        if area.height >= 1 && area.width >= 16 {
            let hint = Line::from(Span::styled(
                "[Esc] to close",
                Style::default().fg(theme.gray_dim),
            ));
            hint.render(Rect::new(area.x, area.y, area.width.min(16), 1), buf);
        }
        return;
    }

    let [_, dialog_h, _] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(dialog_width),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(area);

    let [_, dialog, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(DIALOG_HEIGHT),
        Constraint::Min(0),
    ])
    .flex(Flex::Center)
    .areas(dialog_h);

    // Draw background
    let bg_style = Style::default().bg(theme.bg_dark);
    for y in dialog.y..dialog.y + dialog.height {
        for x in dialog.x..dialog.x + dialog.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(bg_style);
            }
        }
    }

    // Draw border
    let border_style = Style::default().fg(theme.gray_dim).bg(theme.bg_dark);
    // Top border
    if let Some(cell) = buf.cell_mut((dialog.x, dialog.y)) {
        cell.set_char('\u{256D}');
        cell.set_style(border_style);
    }
    for x in dialog.x + 1..dialog.x + dialog.width - 1 {
        if let Some(cell) = buf.cell_mut((x, dialog.y)) {
            cell.set_char('\u{2500}');
            cell.set_style(border_style);
        }
    }
    if let Some(cell) = buf.cell_mut((dialog.x + dialog.width - 1, dialog.y)) {
        cell.set_char('\u{256E}');
        cell.set_style(border_style);
    }
    // Bottom border
    let bottom = dialog.y + dialog.height - 1;
    if let Some(cell) = buf.cell_mut((dialog.x, bottom)) {
        cell.set_char('\u{2570}');
        cell.set_style(border_style);
    }
    for x in dialog.x + 1..dialog.x + dialog.width - 1 {
        if let Some(cell) = buf.cell_mut((x, bottom)) {
            cell.set_char('\u{2500}');
            cell.set_style(border_style);
        }
    }
    if let Some(cell) = buf.cell_mut((dialog.x + dialog.width - 1, bottom)) {
        cell.set_char('\u{256F}');
        cell.set_style(border_style);
    }
    // Side borders
    for y in dialog.y + 1..dialog.y + dialog.height - 1 {
        if let Some(cell) = buf.cell_mut((dialog.x, y)) {
            cell.set_char('\u{2502}');
            cell.set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((dialog.x + dialog.width - 1, y)) {
            cell.set_char('\u{2502}');
            cell.set_style(border_style);
        }
    }

    let inner_x = dialog.x + 2;
    let inner_width = dialog.width.saturating_sub(INNER_PAD);

    // Row 1: Title
    let title = Line::from(Span::styled(
        "New Worktree",
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    ));
    title.render(Rect::new(inner_x, dialog.y + 1, inner_width, 1), buf);

    // Row 2: Label input.
    let prefix_w = LABEL_PREFIX.width() as u16;
    let input_width = inner_width.saturating_sub(prefix_w);
    let viewport = state.viewport(input_width as usize);
    let visible_input = &state.label()[viewport.visible_byte_range];

    let prefix_span = Span::styled(LABEL_PREFIX, Style::default().fg(theme.gray_bright));
    let input_span = Span::styled(visible_input, Style::default().fg(theme.text_primary));
    let input_line = Line::from(vec![prefix_span, input_span]);
    input_line.render(Rect::new(inner_x, dialog.y + 2, inner_width, 1), buf);
    if input_width > 0 {
        let cursor_x = inner_x + prefix_w + viewport.cursor_display_column as u16;
        if let Some(cell) = buf.cell_mut((cursor_x, dialog.y + 2)) {
            cell.set_style(Style::default().fg(theme.bg_dark).bg(theme.text_primary));
        }
    }

    // Row 3: Hints
    let hints = Line::from(vec![
        Span::styled(
            "enter",
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" = create   ", Style::default().fg(theme.gray)),
        Span::styled(
            "esc",
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" = cancel", Style::default().fg(theme.gray)),
    ]);
    hints.render(Rect::new(inner_x, dialog.y + 3, inner_width, 1), buf);
}

/// Dialog width that fits the typed label, clamped to the available area.
fn dialog_width_for(area_width: u16, label: &str) -> u16 {
    let max_width = area_width.saturating_sub(4);
    // prefix + label + block cursor + inner pad
    let needed = (LABEL_PREFIX.width() + label.width() + 1 + INNER_PAD as usize) as u16;
    needed.max(MIN_DIALOG_WIDTH).min(max_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn render_to_text(area: Rect, label: &str) -> String {
        let mut buf = Buffer::empty(area);
        let mut state = NewWorktreeDialogState::new();
        state.set_label(label);
        render_new_worktree_dialog(area, &mut buf, &state);
        let mut lines = Vec::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            lines.push(row);
        }
        lines.join("\n")
    }

    #[test]
    fn empty_dialog_uses_minimum_width() {
        assert_eq!(dialog_width_for(120, ""), MIN_DIALOG_WIDTH);
        assert_eq!(dialog_width_for(40, ""), 36); // area.width - 4
    }

    #[test]
    fn dialog_grows_with_long_label() {
        let label = "a-very-long-worktree-name-that-exceeds-fifty";
        let width = dialog_width_for(120, label);
        assert!(
            width > MIN_DIALOG_WIDTH,
            "expected dialog wider than min for long label, got {width}"
        );
        // Full label + chrome must fit inside the grown dialog.
        let inner = width.saturating_sub(INNER_PAD) as usize;
        let needed = LABEL_PREFIX.width() + label.width() + 1;
        assert!(
            needed <= inner,
            "grown dialog inner={inner} should fit needed={needed}"
        );
    }

    #[test]
    fn dialog_clamps_to_terminal_width() {
        let label = "x".repeat(100);
        let width = dialog_width_for(60, &label);
        assert_eq!(width, 56); // 60 - 4
    }

    #[test]
    fn long_name_fully_visible_on_wide_terminal() {
        let area = Rect::new(0, 0, 100, 20);
        let label = "biscuit-worktree-popup-long-name-fix";
        let text = render_to_text(area, label);
        assert!(
            text.contains(label),
            "full long name must be visible on a wide terminal:\n{text}"
        );
        assert!(text.contains("New Worktree"), "title missing:\n{text}");
    }

    #[test]
    fn long_name_end_visible_on_narrow_terminal() {
        // Terminal narrower than the full label — end (cursor side) must show.
        let area = Rect::new(0, 0, 40, 12);
        let label = "super-long-worktree-name-that-will-not-fit";
        let text = render_to_text(area, label);
        let tail = &label[label.len().saturating_sub(8)..];
        assert!(
            text.contains(tail),
            "end of long name must remain visible when scrolled:\n{text}"
        );
        assert!(
            text.contains('…') || text.contains(tail),
            "expected scrolled indicator or tail:\n{text}"
        );
    }

    #[test]
    fn narrow_dialog_keeps_middle_unicode_cursor_visible() {
        let area = Rect::new(0, 0, 40, 12);
        let grapheme = "👩🏽\u{200d}💻";
        let label = format!("xxxxxxxxxxxx中e\u{301}{grapheme}tail");
        let mut state = NewWorktreeDialogState::new();
        state.set_label(&label);
        let cursor_byte = "xxxxxxxxxxxx中e\u{301}".len();
        let _ = state.set_cursor_byte(cursor_byte);
        let mut buffer = Buffer::empty(area);
        render_new_worktree_dialog(area, &mut buffer, &state);

        assert!(
            (0..area.height).any(|y| {
                (0..area.width).any(|x| buffer[(x, y)].bg == Theme::current().text_primary)
            }),
            "live cursor cell must remain visible",
        );
    }
}
