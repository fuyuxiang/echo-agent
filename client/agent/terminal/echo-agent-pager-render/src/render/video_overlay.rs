//! Video playback overlay chrome (border, title, progress bar).
//!
//! The video frame itself is rendered via post-flush escape sequences
//! by the caller, matching the image viewer pattern.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::prompt_images::VideoViewerState;
use crate::render::safe_buf::SafeBuf;

/// Render the video viewer popup chrome. Returns the popup `Rect`,
/// or `None` if the area is too small.
pub fn render_video_overlay(
    buf: &mut Buffer,
    area: Rect,
    viewer: &VideoViewerState,
    bg: Color,
    text_fg: Color,
    border_fg: Color,
) -> Option<Rect> {
    if area.height < 8 || area.width < 20 {
        return None;
    }

    crate::render::color::dim_area(buf, area, bg, 0.5);

    // 90% centered popup.
    let popup_width = ((area.width as u32 * 90) / 100)
        .max(28)
        .min(area.width as u32) as u16;
    let popup_height = ((area.height as u32 * 90) / 100)
        .max(8)
        .min(area.height as u32) as u16;
    let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);

    ratatui::widgets::Clear.render(popup_rect, buf);
    buf.set_style(popup_rect, Style::default().fg(text_fg).bg(bg));

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_fg).bg(bg))
        .style(Style::default().bg(bg))
        .render(popup_rect, buf);

    // Title centered in top border.
    let title = match viewer.title {
        Some(ref name) => format!(
            " {} ({}\u{00d7}{}) ",
            name, viewer.video_width, viewer.video_height
        ),
        None => format!(
            " Video ({}\u{00d7}{}) ",
            viewer.video_width, viewer.video_height
        ),
    };
    let title_style = Style::default()
        .fg(text_fg)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let tw = title.len() as u16;
    let tx = popup_rect.x + (popup_rect.width.saturating_sub(tw)) / 2;
    buf.set_span_safe(tx, popup_rect.y, &Span::styled(&title, title_style), tw);

    // Progress bar on the bottom border row.
    render_progress_bar(buf, popup_rect, viewer, text_fg, border_fg, bg);

    Some(popup_rect)
}

/// Render the progress bar on the popup's bottom border row.
fn render_progress_bar(
    buf: &mut Buffer,
    popup_rect: Rect,
    viewer: &VideoViewerState,
    text_fg: Color,
    bar_dim: Color,
    bg: Color,
) {
    let bar_y = popup_rect.y + popup_rect.height.saturating_sub(1);
    let inner_width = popup_rect.width.saturating_sub(2) as usize;
    if inner_width <= 10 {
        return;
    }

    let icon = if viewer.playing {
        "\u{25b6}"
    } else {
        "\u{23f8}"
    };
    let time_label = format!(
        "{icon} {}/{}  ",
        format_time(viewer.position_secs()),
        format_time(viewer.duration_secs),
    );
    let bar_width = inner_width.saturating_sub(time_label.len());
    if bar_width <= 4 {
        return;
    }

    let filled = ((viewer.progress() * bar_width as f64).round() as usize).min(bar_width);
    let empty = bar_width.saturating_sub(filled);

    let line = Line::from(vec![
        Span::styled(time_label, Style::default().fg(text_fg).bg(bg)),
        Span::styled(
            "\u{2501}".repeat(filled),
            Style::default().fg(text_fg).bg(bg),
        ),
        Span::styled(
            "\u{2500}".repeat(empty),
            Style::default().fg(bar_dim).bg(bg),
        ),
    ]);

    buf.set_line_safe(popup_rect.x + 1, bar_y, &line, inner_width as u16);
}

fn format_time(secs: f64) -> String {
    let total = secs.round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_zero() {
        assert_eq!(format_time(0.0), "0:00");
    }

    #[test]
    fn format_time_short() {
        assert_eq!(format_time(5.4), "0:05");
    }

    #[test]
    fn format_time_minutes() {
        assert_eq!(format_time(90.0), "1:30");
    }
}
