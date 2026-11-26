//! Theme-agnostic chrome for debug overlays (scroll HUD, FPS HUD).
//!
//! Debug overlays float above themed content and must read identically on
//! every theme. Theme-relative styling fails on dark palettes — Oscura
//! Midnight's base background is `#030304`, so a panel that inherits the
//! theme background (or paints low-contrast foregrounds like `Color::Gray`)
//! blends straight into the frame behind it. These styles pin explicit
//! ANSI-16 colors (never the theme palette, never `Color::Reset`, which
//! defers to the terminal default) and build on [`Style::reset()`] so every
//! painted cell also sheds the modifiers (bold/dim/italic/underline) of
//! whatever themed text it covers.
//!
//! Contract: apply one of these styles to EVERY cell of the overlay rect,
//! trailing padding included, so no themed cell bleeds through the panel.
//! [`render_panel`] is the shared scaffold that enforces it structurally —
//! overlays build lines and call it rather than hand-painting cells.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Body text: white on black, all inherited modifiers cleared.
pub fn overlay_body() -> Style {
    Style::reset().fg(Color::White).bg(Color::Black)
}

/// Title/emphasis text: yellow on black, all inherited modifiers cleared.
pub fn overlay_title() -> Style {
    Style::reset().fg(Color::Yellow).bg(Color::Black)
}

/// Paint a debug panel hugging `area`'s right edge, `top_offset` rows down:
/// the first line in the title style, the rest in the body style, every
/// line truncated/padded to `width` (clamped to the area) so the explicit
/// debug chrome covers the whole rect.
pub fn render_panel(area: Rect, buf: &mut Buffer, top_offset: u16, width: u16, lines: &[&str]) {
    let w = width.min(area.width);
    if w == 0 {
        return;
    }
    let x = area.x + area.width - w;
    let y0 = area.y.saturating_add(top_offset);
    let bottom = area.y + area.height;
    if y0 >= bottom {
        return;
    }
    let h = (lines.len() as u16).min(bottom - y0);
    // Pre-fill the panel rect so the explicit debug bg owns every cell.
    buf.set_style(
        Rect {
            x,
            y: y0,
            width: w,
            height: h,
        },
        overlay_body(),
    );
    for (i, line) in lines.iter().take(h as usize).enumerate() {
        // Pad to the panel width so the background forms a solid block.
        let mut text: String = line.chars().take(w as usize).collect();
        for _ in text.chars().count()..w as usize {
            text.push(' ');
        }
        let style = if i == 0 {
            overlay_title()
        } else {
            overlay_body()
        };
        buf.set_string(x, y0 + i as u16, &text, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    /// The styles must carry explicit ANSI-16 colors and subtract every
    /// modifier — `Style::reset()` is the mechanism that clears themed
    /// bold/italic/dim from covered cells.
    #[test]
    fn styles_are_explicit_and_modifier_clearing() {
        for (style, fg) in [
            (overlay_body(), Color::White),
            (overlay_title(), Color::Yellow),
        ] {
            assert_eq!(style.fg, Some(fg));
            assert_eq!(style.bg, Some(Color::Black));
            assert_eq!(style.add_modifier, Modifier::empty());
            assert_eq!(style.sub_modifier, Modifier::all());
        }
    }
}
