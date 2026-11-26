//! `/gboom` easter-egg overlay chrome (border, title, HUD bar).
//!
//! The game frame itself is rendered via post-flush kitty escape sequences
//! by the caller, matching the image/video viewer pattern.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::gboom::GboomHud;
use crate::render::safe_buf::SafeBuf;

/// Render the GBOOM popup chrome. Returns the popup `Rect`,
/// or `None` if the area is too small to play in.
pub fn render_gboom_overlay(
    buf: &mut Buffer,
    area: Rect,
    hud: &GboomHud,
    bg: Color,
    text_fg: Color,
    border_fg: Color,
) -> Option<Rect> {
    if area.height < 8 || area.width < 30 {
        return None;
    }

    crate::render::color::dim_area(buf, area, bg, 0.5);

    // 90% centered popup, like the video viewer.
    let popup_width = ((area.width as u32 * 90) / 100)
        .max(30)
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

    // Title centered in the top border, in the iconic logo red.
    let title = " GBOOM ";
    let [r, g, b] = crate::gboom::GBOOM_RED;
    let title_style = Style::default()
        .fg(Color::Rgb(r, g, b))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let tw = title.len() as u16;
    let tx = popup_rect.x + (popup_rect.width.saturating_sub(tw)) / 2;
    buf.set_span_safe(tx, popup_rect.y, &Span::styled(title, title_style), tw);

    // HUD on the bottom border row.
    render_hud_bar(buf, popup_rect, hud, border_fg, bg);

    Some(popup_rect)
}

/// Render the HUD on the popup's bottom border row:
/// `HP 100 · KILLS 0/8` left, controls hint right.
fn render_hud_bar(buf: &mut Buffer, popup_rect: Rect, hud: &GboomHud, dim_fg: Color, bg: Color) {
    let bar_y = popup_rect.y + popup_rect.height.saturating_sub(1);
    let inner_width = popup_rect.width.saturating_sub(2) as usize;
    if inner_width <= 12 {
        return;
    }

    // Health-bar semantics: green when comfortable, amber when hurting,
    // GBOOM red when critical.
    let hp_color = if hud.hp > 60 {
        Color::Rgb(126, 200, 96)
    } else if hud.hp > 30 {
        Color::Rgb(235, 198, 82)
    } else {
        let [r, g, b] = crate::gboom::GBOOM_RED;
        Color::Rgb(r, g, b)
    };
    let stats = format!(
        " HP {:<3} \u{00b7} KILLS {}/{} ",
        hud.hp, hud.kills, hud.total
    );
    // `chars().count()` not `len()`: the separator is multi-byte UTF-8 but
    // every char here is a single display cell.
    let stats_w = (stats.chars().count() as u16).min(inner_width as u16);
    let line = Line::from(vec![Span::styled(
        stats,
        Style::default()
            .fg(hp_color)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )]);
    buf.set_line_safe(popup_rect.x + 1, bar_y, &line, stats_w);

    let hint = if hud.playing {
        " WASD/\u{2190}\u{2192} move \u{00b7} SPACE fire \u{00b7} ESC quit "
    } else {
        " ESC quit "
    };
    let hint_w = hint.chars().count() as u16;
    if (stats_w + hint_w) as usize <= inner_width {
        let hx = popup_rect.x + 1 + inner_width as u16 - hint_w;
        buf.set_span_safe(
            hx,
            bar_y,
            &Span::styled(hint, Style::default().fg(dim_fg).bg(bg)),
            hint_w,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hud() -> GboomHud {
        GboomHud {
            hp: 100,
            kills: 2,
            total: 8,
            playing: true,
        }
    }

    #[test]
    fn returns_none_when_area_too_small() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        assert!(
            render_gboom_overlay(
                &mut buf,
                Rect::new(0, 0, 20, 5),
                &hud(),
                Color::Black,
                Color::White,
                Color::Gray,
            )
            .is_none()
        );
    }

    #[test]
    fn renders_popup_with_title_and_hud() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let popup = render_gboom_overlay(
            &mut buf,
            area,
            &hud(),
            Color::Black,
            Color::White,
            Color::Gray,
        )
        .expect("popup should render");
        assert!(popup.width >= 30);

        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("GBOOM"), "title missing");
        assert!(content.contains("KILLS"), "HUD missing");
    }
}
