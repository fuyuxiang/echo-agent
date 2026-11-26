//! Unicode block progress bar at 1/8th-cell resolution via the LEFT
//! fractional blocks `▏▎▍▌▋▊▉█`.
//!
//! Consolas (the default ConHost font) is missing the narrow ones
//! (U+258F..=U+2589) — see microsoft/terminal#387 — so on legacy
//! ConHost we substitute the shade glyphs `░▒▓` from CP437 instead.
//! Same eighth-resolution input; the cell just reads as a density
//! pattern rather than a true left-justified bar.
//!
//! ```ignore
//! render_progress_bar(buf, x, y, 5, 0.42, fg_color, bg_color);
//! ```

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// LEFT-fractional block glyphs, indexed 0–8 (0 = empty, 8 = full).
const BLOCKS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// Shade substitutes used on hosts that can't render the LEFT-fractional
/// blocks. Same index domain as [`BLOCKS`] so call sites stay uniform.
const SHADES: [&str; 9] = ["", "░", "░", "░", "▒", "▒", "▓", "▓", "█"];

/// Per-cell partial-fill glyph table — `BLOCKS` everywhere except legacy
/// ConHost, where we substitute `SHADES`.
fn partial_blocks() -> &'static [&'static str; 9] {
    if crate::glyphs::is_legacy_windows_console() {
        &SHADES
    } else {
        &BLOCKS
    }
}

/// Split a fill fraction into (whole cells, remainder eighths).
fn cell_breakdown(width: u16, value: f32) -> (u16, usize) {
    let value = value.clamp(0.0, 1.0);
    let total_eighths = (value * width as f32 * 8.0).round() as u16;
    let full = (total_eighths / 8).min(width);
    let remainder = (total_eighths % 8) as usize;
    (full, remainder)
}

/// Per-cell `(symbol, is_filled)` for a `width`-cell bar at `value`
/// fill. Single source of truth for both renderers below.
fn bar_cells(width: u16, value: f32) -> impl Iterator<Item = (&'static str, /* filled */ bool)> {
    let (full, remainder) = cell_breakdown(width, value);
    let glyphs = partial_blocks();
    (0..width).map(move |i| {
        if i < full {
            (glyphs[8], true)
        } else if i == full && remainder > 0 {
            (glyphs[remainder], true)
        } else {
            (" ", false)
        }
    })
}

/// Build a progress bar as styled spans (one per cell).
///
/// Each span has `fg` on `bg`, suitable for composing into a `Line`.
pub fn progress_bar_spans(width: u16, value: f32, fg: Color, bg: Color) -> Vec<Span<'static>> {
    let fg_style = Style::default().fg(fg).bg(bg);
    let bg_style = Style::default().bg(bg);
    bar_cells(width, value)
        .map(|(symbol, filled)| {
            let style = if filled { fg_style } else { bg_style };
            Span::styled(symbol.to_string(), style)
        })
        .collect()
}

/// Render a progress bar into the buffer at the given position.
///
/// - `width`: number of character cells for the bar
/// - `value`: fill fraction in `0.0..=1.0` (clamped)
/// - `fg`: color for the filled portion
/// - `bg`: background color for the track (filled + empty cells)
pub fn render_progress_bar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    value: f32,
    fg: Color,
    bg: Color,
) {
    let fg_style = Style::default().fg(fg).bg(bg);
    let bg_style = Style::default().bg(bg);
    for (i, (symbol, filled)) in bar_cells(width, value).enumerate() {
        let Some(cell) = buf.cell_mut((x + i as u16, y)) else {
            continue;
        };
        cell.set_symbol(symbol);
        cell.set_style(if filled { fg_style } else { bg_style });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn test_empty_bar() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        render_progress_bar(&mut buf, 0, 0, 5, 0.0, Color::White, Color::Black);
        for i in 0..5u16 {
            assert_eq!(buf[(i, 0)].symbol(), " ");
        }
    }

    #[test]
    fn test_full_bar() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        render_progress_bar(&mut buf, 0, 0, 5, 1.0, Color::White, Color::Black);
        for i in 0..5u16 {
            assert_eq!(buf[(i, 0)].symbol(), "█");
        }
    }

    #[test]
    fn test_half_bar() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        render_progress_bar(&mut buf, 0, 0, 4, 0.5, Color::White, Color::Black);
        // 50% of 4 cells = 2 full blocks
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(1, 0)].symbol(), "█");
        assert_eq!(buf[(2, 0)].symbol(), " ");
        assert_eq!(buf[(3, 0)].symbol(), " ");
    }

    #[test]
    fn test_partial_block() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        // 25% of 4 cells = 1 full block (8 eighths). Actually 0.25*4*8 = 8 = 1 full.
        // Let's use 12.5% of 4 cells = 0.125*4*8 = 4 eighths = half block on cell 0
        render_progress_bar(&mut buf, 0, 0, 4, 0.125, Color::White, Color::Black);
        assert_eq!(buf[(0, 0)].symbol(), "▌"); // 4/8 = half
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn cell_breakdown_keeps_eighths_resolution() {
        // 0.5 * 4 * 8 = 16 eighths → 2 full + 0 remainder.
        assert_eq!(cell_breakdown(4, 0.5), (2, 0));
        // 0.125 * 4 * 8 = 4 eighths → 0 full + 4 remainder.
        assert_eq!(cell_breakdown(4, 0.125), (0, 4));
        // 0.03 * 5 * 8 = 1.2 → rounds to 1 eighth → 0 full + 1 remainder.
        // On legacy that picks SHADES[1] = "░"; on truecolor it picks
        // BLOCKS[1] = "▏". Either way ~3% does NOT light a full cell.
        assert_eq!(cell_breakdown(5, 0.03), (0, 1));
        // Out-of-range clamped.
        assert_eq!(cell_breakdown(4, 2.0), (4, 0));
    }

    #[test]
    fn shades_and_blocks_tables_match_in_length() {
        // The two glyph tables must share the same index domain so call
        // sites can swap them without branching on the host.
        assert_eq!(BLOCKS.len(), SHADES.len());
        assert_eq!(BLOCKS[0], SHADES[0]); // both empty
        assert_eq!(BLOCKS[8], SHADES[8]); // both full block
    }
}
