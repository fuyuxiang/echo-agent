//! Color blending and fading utilities.
//!
//! These utilities support smooth fade transitions (e.g., for sticky headers
//! being pushed off screen) by blending colors toward a base color.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};

/// The 6 channel values in the 256-color 6×6×6 cube.
const CUBE_VALUES: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Convert a 256-color indexed color to its (R, G, B) components.
///
/// Handles all three regions of the 256-color palette:
/// - 0–15:    standard/bright ANSI colors (uses common xterm defaults)
/// - 16–231:  6×6×6 color cube
/// - 232–255: 24-step grayscale ramp
pub fn indexed_to_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        // Standard colors (0–7) — common xterm defaults
        0 => (0, 0, 0),
        1 => (128, 0, 0),
        2 => (0, 128, 0),
        3 => (128, 128, 0),
        4 => (0, 0, 128),
        5 => (128, 0, 128),
        6 => (0, 128, 128),
        7 => (192, 192, 192),
        // Bright colors (8–15)
        8 => (128, 128, 128),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (0, 0, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        // 6×6×6 color cube (16–231)
        16..=231 => {
            let n = index - 16;
            let r = CUBE_VALUES[(n / 36) as usize];
            let g = CUBE_VALUES[((n % 36) / 6) as usize];
            let b = CUBE_VALUES[(n % 6) as usize];
            (r, g, b)
        }
        // Grayscale ramp (232–255): value = 8 + (index − 232) × 10
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

/// Map an RGB triplet to the nearest 256-color palette index (16–255).
///
/// Searches both the 6×6×6 color cube (16–231) and the 24-step grayscale
/// ramp (232–255), returning whichever has the smallest squared Euclidean
/// distance.
pub fn nearest_indexed(r: u8, g: u8, b: u8) -> u8 {
    // --- nearest in the 6×6×6 color cube (16–231) ---
    let ri = nearest_cube_channel(r);
    let gi = nearest_cube_channel(g);
    let bi = nearest_cube_channel(b);
    let cube_idx = 16 + 36 * ri as u16 + 6 * gi as u16 + bi as u16;
    let cube_dist = sq_dist(
        r,
        g,
        b,
        CUBE_VALUES[ri as usize],
        CUBE_VALUES[gi as usize],
        CUBE_VALUES[bi as usize],
    );

    // --- nearest in the grayscale ramp (232–255) ---
    // Ramp values: 8, 18, 28, …, 238  (24 entries)
    let lum = (r as u16 + g as u16 + b as u16) / 3;
    let gray_step = if lum <= 3 {
        0u8
    } else if lum >= 243 {
        23
    } else {
        ((lum as i16 - 8 + 5) / 10).clamp(0, 23) as u8
    };
    let gv = (8 + gray_step as u16 * 10) as u8;
    let gray_dist = sq_dist(r, g, b, gv, gv, gv);

    if gray_dist < cube_dist {
        232 + gray_step
    } else {
        cube_idx as u8
    }
}

/// Find the nearest index (0–5) into [`CUBE_VALUES`] for a single channel.
fn nearest_cube_channel(v: u8) -> u8 {
    let mut best = 0u8;
    let mut best_d = v.abs_diff(CUBE_VALUES[0]) as u16;
    for i in 1..6u8 {
        let d = v.abs_diff(CUBE_VALUES[i as usize]) as u16;
        if d < best_d {
            best = i;
            best_d = d;
        }
    }
    best
}

/// Squared Euclidean distance between two RGB colors.
fn sq_dist(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    (dr * dr + dg * dg + db * db) as u32
}

/// Extract (R, G, B) from a Color, supporting both Rgb and Indexed variants.
///
/// Returns `None` for named ANSI colors (Color::Red, etc.) and Color::Reset.
fn color_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(n) => Some(indexed_to_rgb(n)),
        _ => None,
    }
}

/// Map every [`Color`] variant to an xterm-default RGB triple. `None`
/// only for `Color::Reset` (no defined RGB — caller chooses a fallback).
///
/// Useful when downstream code must produce RGB for *every* color value
/// — e.g. progress-bar gradients that lerp across named breakpoints, or
/// OSC 12 cursor-color updates that must emit an RGB triple regardless
/// of terminal color depth.
///
/// Named-color RGB matches the xterm 16-color palette used by
/// [`indexed_to_rgb`] for indices 0–15; the user's terminal may have
/// customised those entries, so the result is "approximate but
/// consistent with our other colorimetry".
pub fn resolve_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    let idx: u8 = match color {
        Color::Rgb(r, g, b) => return Some((r, g, b)),
        Color::Indexed(n) => return Some(indexed_to_rgb(n)),
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Reset => return None,
    };
    Some(indexed_to_rgb(idx))
}

/// Blend a single color channel: lerp from base toward original based on opacity.
///
/// - `opacity = 0.0`: returns `base` (fully faded)
/// - `opacity = 1.0`: returns `original` (no change)
#[inline]
pub fn blend_channel(base: u8, original: u8, opacity: f32) -> u8 {
    // result = base + (original - base) * opacity
    //        = base * (1 - opacity) + original * opacity
    let result = base as f32 * (1.0 - opacity) + original as f32 * opacity;
    result.round() as u8
}

/// Blend a color toward a base color based on opacity.
///
/// - `opacity = 0.0`: returns `base` (fully faded)
/// - `opacity = 1.0`: returns `original` (no change)
///
/// Supports both `Color::Rgb` and `Color::Indexed` colors (indexed colors are
/// converted to their RGB equivalents for blending). When either input is
/// `Color::Indexed`, the blended result is quantized back to the nearest
/// 256-color index so the output stays terminal-compatible.
///
/// Returns `None` for named ANSI colors (Color::Red, etc.) since their RGB
/// values are terminal-dependent.
pub fn blend_color(base: Color, original: Color, opacity: f32) -> Option<Color> {
    let (base_r, base_g, base_b) = color_to_rgb(base)?;
    let (orig_r, orig_g, orig_b) = color_to_rgb(original)?;

    let r = blend_channel(base_r, orig_r, opacity);
    let g = blend_channel(base_g, orig_g, opacity);
    let b = blend_channel(base_b, orig_b, opacity);

    // When either input is indexed, quantize the blended result back to the
    // nearest 256-color index so the output stays terminal-compatible.
    // On 256-color terminals the theme quantizes all colors to Indexed at
    // startup, so any Indexed input signals that the terminal cannot handle
    // raw RGB — the output must stay in the indexed palette.
    Some(match (base, original) {
        (Color::Indexed(_), _) | (_, Color::Indexed(_)) => Color::Indexed(nearest_indexed(r, g, b)),
        _ => Color::Rgb(r, g, b),
    })
}

/// Blend all span colors in a line toward a base color.
///
/// This is useful for making content appear "faded" or "muted" by blending
/// its colors toward the background.
///
/// - `opacity = 0.0`: fully faded to base color
/// - `opacity = 1.0`: no change (original colors)
///
/// Named ANSI colors are left unchanged.
pub fn blend_line(line: Line<'static>, base: Color, opacity: f32) -> Line<'static> {
    let blended_spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            if let Some(fg) = style.fg
                && let Some(blended) = blend_color(base, fg, opacity)
            {
                style.fg = Some(blended);
            }
            Span::styled(span.content, style)
        })
        .collect();
    Line::from(blended_spans).style(line.style)
}

/// Blend all span colors in a line toward a base color, with default foreground.
///
/// Like `blend_line`, but spans without an explicit fg color are assigned
/// `default_fg` before blending. This ensures all text gets blended, not just
/// explicitly colored text.
///
/// - `opacity = 0.0`: fully faded to base color
/// - `opacity = 1.0`: no change (original colors)
///
/// Named ANSI colors are left unchanged.
pub fn blend_line_with_default(
    line: Line<'static>,
    base: Color,
    default_fg: Color,
    opacity: f32,
) -> Line<'static> {
    let blended_spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            // Use default_fg if no explicit fg color
            let fg = style.fg.unwrap_or(default_fg);
            if let Some(blended) = blend_color(base, fg, opacity) {
                style.fg = Some(blended);
            }
            Span::styled(span.content, style)
        })
        .collect();
    Line::from(blended_spans).style(line.style)
}

/// Fade a region of the buffer toward a base color.
///
/// This blends both foreground and background colors of each cell toward
/// `base_color` based on `opacity`:
/// - `opacity = 0.0`: fully faded (cells become base_color)
/// - `opacity = 1.0`: no change
///
/// Both RGB and Indexed colors are blended; named ANSI colors (Color::Red, etc.)
/// are left unchanged since their RGB values are terminal-dependent.
pub fn fade_region(buf: &mut Buffer, area: Rect, base_color: Color, opacity: f32) {
    blend_area(
        buf,
        area,
        Some((base_color, opacity)),
        Some((base_color, opacity)),
    );
}

/// Blend fg and/or bg of every cell in an area toward target colors.
///
/// Each parameter is `Option<(target, opacity)>`:
/// - `None`: leave that channel unchanged
/// - `Some((target, opacity))`: blend toward `target` at `opacity`
///   - `opacity = 0.0`: fully target (original gone)
///   - `opacity = 1.0`: no change (original kept)
///
/// Both RGB and Indexed colors are blended; named ANSI color cells are skipped.
pub fn blend_area(
    buf: &mut Buffer,
    area: Rect,
    fg: Option<(Color, f32)>,
    bg: Option<(Color, f32)>,
) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                if let Some((target, opacity)) = fg
                    && let Some(blended) = blend_color(target, cell.fg, opacity)
                {
                    cell.set_fg(blended);
                }
                if let Some((target, opacity)) = bg
                    && let Some(blended) = blend_color(target, cell.bg, opacity)
                {
                    cell.set_bg(blended);
                }
            }
        }
    }
}

/// Dim a screen area: reset all modifiers then blend toward a background color.
///
/// This ensures no bold/italic/underline bleeds through the dimmed overlay.
pub fn dim_area(buf: &mut Buffer, area: Rect, blend_bg: ratatui::style::Color, blend_factor: f32) {
    use ratatui::style::Modifier;

    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                // Strip all modifiers (BOLD, ITALIC, UNDERLINE, etc.).
                cell.modifier = Modifier::empty();
            }
        }
    }
    // Then blend colors.
    crate::render::color::blend_area(buf, area, Some((blend_bg, blend_factor)), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nearest_indexed_exact_cube_values() {
        // Pure black in the cube → index 16
        assert_eq!(nearest_indexed(0, 0, 0), 16);
        // Pure white in the cube → index 231
        assert_eq!(nearest_indexed(255, 255, 255), 231);
        // Exact cube hit: rgb(95, 135, 215) → 16 + 36*1 + 6*2 + 4 = 68
        assert_eq!(nearest_indexed(95, 135, 215), 68);
    }

    #[test]
    fn test_nearest_indexed_grayscale() {
        // Mid-gray should map to a grayscale index
        let idx = nearest_indexed(128, 128, 128);
        assert!((232..=255).contains(&idx));
    }

    #[test]
    fn test_nearest_indexed_roundtrip() {
        // A known indexed color should round-trip back to itself
        for &idx in &[16u8, 141, 149, 210, 234, 243, 245, 255] {
            let (r, g, b) = indexed_to_rgb(idx);
            assert_eq!(
                nearest_indexed(r, g, b),
                idx,
                "round-trip failed for index {idx}"
            );
        }
    }

    #[test]
    fn test_blend_channel_extremes() {
        // opacity = 0: fully base
        assert_eq!(blend_channel(0, 255, 0.0), 0);
        assert_eq!(blend_channel(100, 200, 0.0), 100);

        // opacity = 1: fully original
        assert_eq!(blend_channel(0, 255, 1.0), 255);
        assert_eq!(blend_channel(100, 200, 1.0), 200);
    }

    #[test]
    fn test_blend_channel_midpoint() {
        // opacity = 0.5: halfway between
        assert_eq!(blend_channel(0, 100, 0.5), 50);
        assert_eq!(blend_channel(100, 200, 0.5), 150);
        assert_eq!(blend_channel(0, 255, 0.5), 128); // 127.5 rounds to 128
    }

    #[test]
    fn test_blend_channel_partial() {
        // 25% opacity
        assert_eq!(blend_channel(0, 100, 0.25), 25);
        // 75% opacity
        assert_eq!(blend_channel(0, 100, 0.75), 75);
    }

    #[test]
    fn test_blend_color_rgb() {
        let base = Color::Rgb(0, 0, 0);
        let original = Color::Rgb(100, 150, 200);

        // Fully faded
        let faded = blend_color(base, original, 0.0);
        assert_eq!(faded, Some(Color::Rgb(0, 0, 0)));

        // No change
        let unchanged = blend_color(base, original, 1.0);
        assert_eq!(unchanged, Some(Color::Rgb(100, 150, 200)));

        // Halfway
        let half = blend_color(base, original, 0.5);
        assert_eq!(half, Some(Color::Rgb(50, 75, 100)));
    }

    #[test]
    fn test_blend_color_indexed_returns_indexed() {
        // Both indexed → result is indexed (quantized back to 256-color palette)
        let base = Color::Indexed(232); // near-black (8, 8, 8)
        let original = Color::Indexed(255); // near-white (238, 238, 238)

        let half = blend_color(base, original, 0.5).unwrap();
        assert!(matches!(half, Color::Indexed(_)));

        // Fully base
        let faded = blend_color(base, original, 0.0).unwrap();
        assert!(matches!(faded, Color::Indexed(_)));

        // Fully original
        let full = blend_color(base, original, 1.0).unwrap();
        assert!(matches!(full, Color::Indexed(_)));
    }

    #[test]
    fn test_blend_color_mixed_returns_indexed() {
        let rgb = Color::Rgb(100, 100, 100);
        let indexed = Color::Indexed(5); // magenta (128, 0, 128)

        // Mixed: indexed base + rgb original → Indexed result (quantized)
        let result = blend_color(indexed, rgb, 0.5);
        assert!(
            matches!(result, Some(Color::Indexed(_))),
            "expected Indexed, got {result:?}"
        );

        // Mixed: rgb base + indexed original → Indexed result (quantized)
        let result = blend_color(rgb, indexed, 0.5);
        assert!(
            matches!(result, Some(Color::Indexed(_))),
            "expected Indexed, got {result:?}"
        );
    }

    #[test]
    fn test_blend_color_named_returns_none() {
        let rgb = Color::Rgb(100, 100, 100);
        let named = Color::Red;

        // Named ANSI colors are not blendable
        assert_eq!(blend_color(named, rgb, 0.5), None);
        assert_eq!(blend_color(rgb, named, 0.5), None);
    }

    #[test]
    fn test_fade_region() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 2));

        // Set up some RGB colors
        let fg_color = Color::Rgb(200, 200, 200);
        let bg_color = Color::Rgb(50, 50, 50);
        let base = Color::Rgb(0, 0, 0);

        for y in 0..2 {
            for x in 0..3 {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_fg(fg_color);
                    cell.set_bg(bg_color);
                }
            }
        }

        // Fade to 50%
        fade_region(&mut buf, Rect::new(0, 0, 3, 2), base, 0.5);

        // Check cells are faded
        if let Some(cell) = buf.cell((0, 0)) {
            assert_eq!(cell.fg, Color::Rgb(100, 100, 100)); // 200 * 0.5
            assert_eq!(cell.bg, Color::Rgb(25, 25, 25)); // 50 * 0.5
        }
    }

    #[test]
    fn test_fade_region_partial_area() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));

        let fg_color = Color::Rgb(100, 100, 100);
        let base = Color::Rgb(0, 0, 0);

        // Set all cells
        for y in 0..4 {
            for x in 0..4 {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_fg(fg_color);
                }
            }
        }

        // Only fade a 2x2 region in the middle
        fade_region(&mut buf, Rect::new(1, 1, 2, 2), base, 0.0);

        // Corner should be unchanged
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Rgb(100, 100, 100));

        // Middle should be fully faded
        assert_eq!(buf.cell((1, 1)).unwrap().fg, Color::Rgb(0, 0, 0));
        assert_eq!(buf.cell((2, 2)).unwrap().fg, Color::Rgb(0, 0, 0));

        // Other corner unchanged
        assert_eq!(buf.cell((3, 3)).unwrap().fg, Color::Rgb(100, 100, 100));
    }

    #[test]
    fn test_blend_area_fg_only() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        let fg = Color::Rgb(200, 100, 0);
        let bg = Color::Rgb(10, 10, 10);
        for x in 0..2 {
            if let Some(cell) = buf.cell_mut((x, 0)) {
                cell.set_fg(fg);
                cell.set_bg(bg);
            }
        }

        let target = Color::Rgb(0, 0, 0);
        blend_area(&mut buf, Rect::new(0, 0, 2, 1), Some((target, 0.5)), None);

        // fg blended to 50%
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Rgb(100, 50, 0));
        // bg unchanged
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(10, 10, 10));
    }

    #[test]
    fn test_blend_area_bg_only() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        let fg = Color::Rgb(200, 200, 200);
        let bg = Color::Rgb(100, 100, 100);
        for x in 0..2 {
            if let Some(cell) = buf.cell_mut((x, 0)) {
                cell.set_fg(fg);
                cell.set_bg(bg);
            }
        }

        let target = Color::Rgb(0, 0, 0);
        blend_area(&mut buf, Rect::new(0, 0, 2, 1), None, Some((target, 0.5)));

        // fg unchanged
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Rgb(200, 200, 200));
        // bg blended to 50%
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(50, 50, 50));
    }

    #[test]
    fn test_blend_area_both() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(Color::Rgb(100, 200, 0));
            cell.set_bg(Color::Rgb(50, 50, 50));
        }

        let fg_target = Color::Rgb(0, 0, 0);
        let bg_target = Color::Rgb(20, 20, 20);
        blend_area(
            &mut buf,
            Rect::new(0, 0, 1, 1),
            Some((fg_target, 0.75)),
            Some((bg_target, 0.75)),
        );

        // fg: 75% of (100,200,0) + 25% of (0,0,0) = (75,150,0)
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Rgb(75, 150, 0));
        // bg: 75% of (50,50,50) + 25% of (20,20,20) = (42.5, 42.5, 42.5) → (43,43,43)
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(43, 43, 43));
    }

    #[test]
    fn test_blend_area_none_none_is_noop() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 2));
        let fg = Color::Rgb(123, 45, 67);
        let bg = Color::Rgb(89, 10, 11);
        for y in 0..2 {
            for x in 0..2 {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_fg(fg);
                    cell.set_bg(bg);
                }
            }
        }

        blend_area(&mut buf, Rect::new(0, 0, 2, 2), None, None);

        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(buf.cell((x, y)).unwrap().fg, fg);
                assert_eq!(buf.cell((x, y)).unwrap().bg, bg);
            }
        }
    }

    #[test]
    fn test_blend_area_named_color_skipped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(Color::Red); // named color — blend_color returns None
            cell.set_bg(Color::Red);
        }

        blend_area(
            &mut buf,
            Rect::new(0, 0, 1, 1),
            Some((Color::Rgb(0, 0, 0), 0.5)),
            Some((Color::Rgb(0, 0, 0), 0.5)),
        );

        // Named colors should be unchanged (blend_color returns None for them)
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Red);
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Red);
    }

    #[test]
    fn test_blend_area_indexed_colors_blended() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(Color::Indexed(255)); // near-white grayscale
            cell.set_bg(Color::Indexed(255));
        }

        // Blend toward black (indexed 232 = #080808, but we use indexed 16 = #000000)
        let target = Color::Indexed(16); // black in the color cube
        blend_area(
            &mut buf,
            Rect::new(0, 0, 1, 1),
            Some((target, 0.5)),
            Some((target, 0.5)),
        );

        // Both should now be blended (and still indexed, not Rgb)
        let cell = buf.cell((0, 0)).unwrap();
        assert!(matches!(cell.fg, Color::Indexed(_)));
        assert!(matches!(cell.bg, Color::Indexed(_)));
    }
}
