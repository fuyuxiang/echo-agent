//! Tip renderer.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::render::SafeBuf;
use crate::theme::Theme;

/// Columns between the composer's border and the text of the rows above it.
pub const HINT_INSET: u16 = 1;

/// Compute the number of rows a tip needs when rendered at the given `width`.
pub fn tip_height(width: u16, tip: &str) -> u16 {
    if width == 0 {
        return 0;
    }
    let line = tip_line(tip);
    let line_width = line.width() as u16;
    if line_width <= width {
        1
    } else {
        // Ceiling division — word wrapping may use slightly more rows than
        // a naive character split, but this is a close-enough upper bound.
        (line_width as u32)
            .div_ceil(width as u32)
            .min(u16::MAX as u32) as u16
    }
}

fn tip_line(tip: &str) -> Line<'_> {
    let theme = Theme::current();
    Line::from(vec![
        Span::styled(
            "Tip: ",
            Style::default().fg(theme.gray).add_modifier(Modifier::BOLD),
        ),
        Span::styled(tip, Style::default().fg(theme.gray)),
    ])
}

/// Rows above the composer line up one column inside its border, not out at its edge.
/// Callers paint the full slot and place text here, so the background still covers column zero.
pub fn hint_text_area(area: Rect) -> Rect {
    Rect {
        x: area.x + HINT_INSET,
        width: area.width.saturating_sub(HINT_INSET),
        ..area
    }
}

/// Render a tip into the provided area, word-wrapping if it exceeds the width.
pub fn render_tip(area: Rect, buf: &mut Buffer, tip: &str, inset: u16) {
    if area.height == 0 {
        return;
    }

    let theme = Theme::current();
    let text = Rect {
        x: area.x + inset,
        width: area.width.saturating_sub(inset),
        ..area
    };

    clear_rect(buf, area, theme.bg_base);
    Paragraph::new(tip_line(tip))
        .style(Style::default().bg(theme.bg_base))
        .wrap(Wrap { trim: false })
        .render(text, buf);
}

/// Blank every cell of `area` (chars, colors, and modifiers) in `color`.
///
/// Modifiers MUST be reset here: ratatui's `Cell::set_style` only *merges*
/// modifiers (`insert(add)` / `remove(sub)`), so a later paint whose style
/// carries no `sub_modifier` inherits whatever BOLD/ITALIC/… an earlier
/// same-frame paint left behind (e.g. the welcome tip's bold `Tip: ` prefix
/// bleeding into the ephemeral tip as "**Queue**d · Enter to send now").
fn clear_rect(buf: &mut Buffer, area: Rect, color: Color) {
    for row in 0..area.height {
        for col in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                cell.set_char(' ');
                cell.fg = color;
                cell.bg = color;
                cell.modifier = Modifier::empty();
            }
        }
    }
}

/// Render a pre-styled tip line into the banner rect. The whole rect is
/// cleared first (it can be taller than one row when a wrapped session tip
/// reserved it) and the line paints on the first row, truncated at width.
pub fn render_ephemeral_tip(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = Theme::current();
    clear_rect(buf, area, theme.bg_base);
    let text = hint_text_area(area);
    buf.set_line_safe(text.x, text.y, line, text.width);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (0..area.width)
            .map(|x| buf.cell((area.x + x, y)).expect("cell in area").symbol())
            .collect()
    }

    #[test]
    fn clears_full_rect_and_truncates_to_width() {
        let area = Rect::new(0, 0, 8, 2);
        let mut buf = Buffer::empty(area);
        // Pre-dirty both rows to simulate stale banner content underneath.
        buf.set_string(0, 0, "XXXXXXXX", Style::default());
        buf.set_string(0, 1, "XXXXXXXX", Style::default());

        let line = Line::from("0123456789"); // wider than the rect
        render_ephemeral_tip(area, &mut buf, &line);

        assert_eq!(
            row_text(&buf, area, 0),
            " 0123456",
            "inset by one, truncated at width"
        );
        assert_eq!(
            row_text(&buf, area, 1),
            "        ",
            "stale rows below the line are cleared"
        );
    }

    #[test]
    fn zero_sized_area_is_a_noop() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "XXXXXXXX", Style::default());
        render_ephemeral_tip(Rect::new(0, 0, 8, 0), &mut buf, &Line::from("tip"));
        assert_eq!(row_text(&buf, area, 0), "XXXXXXXX", "untouched");
    }

    /// Regression: a bold underpaint in the banner rect (e.g. the welcome
    /// tip's `Tip: ` prefix painted the same frame) must not bleed BOLD into
    /// the ephemeral tip. `Cell::set_style` merges modifiers, so the clear
    /// pass has to reset them explicitly — otherwise `Queued · Enter …`
    /// rendered as bold `Queue` + regular `d` (5 leaked bold cells).
    #[test]
    fn clears_leaked_modifiers_from_underpaint() {
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        // Simulate the phantom session-tip underpaint: 5 bold cells ("Tip: ").
        buf.set_string(
            0,
            0,
            "Tip: never gonna give you up",
            Style::default().add_modifier(Modifier::BOLD),
        );

        // The send-now tip shape: dim text with a single bold key chord.
        let dim = Style::default();
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::styled("Queued · ", dim),
            Span::styled("Enter", bold),
            Span::styled(" to send now", dim),
        ]);
        render_ephemeral_tip(area, &mut buf, &line);

        assert_eq!(row_text(&buf, area, 0).trim(), "Queued · Enter to send now");
        let bold_cols: Vec<u16> = (0..area.width)
            .filter(|&x| {
                buf.cell((x, 0))
                    .expect("cell in area")
                    .modifier
                    .contains(Modifier::BOLD)
            })
            .collect();
        // Inset by one: "Queued · " occupies cols 1..10, "Enter" cols 10..15.
        assert_eq!(
            bold_cols,
            (10..15).collect::<Vec<u16>>(),
            "only the Enter chord may be bold — no leak from the underpaint"
        );
    }
}
