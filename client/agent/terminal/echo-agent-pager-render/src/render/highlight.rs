//! Match-highlight overlay shared by the list pane and other search surfaces.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::bidi::{is_enabled, logical_cols_to_visual, needs_bidi};
use crate::render::wrapping::{
    byte_offset_to_display_col, byte_range_to_row_cols, wrap_byte_ranges_matching,
};

/// Invert (REVERSED) the buffer cells covering every match of `re` in `text`.
///
/// Run as a post-pass after a line has been drawn, so matches are highlighted
/// regardless of the underlying colors.
///
/// - `area`: the pane area; `area.x` / `area.width` bound painting horizontally.
/// - `row_y`: buffer row of the line's first visible row.
/// - `viewport_bottom`: exclusive bottom row; wrapped rows at or below it stop.
/// - `skip`: leading wrapped rows of this line clipped above the viewport.
/// - `prefix_w`: display column where `text` begins (e.g. a line-number gutter).
/// - `text`: plain text the regex runs against (logical order; RTL matches are
///   mapped to visual columns to match painted cells).
/// - `single_row`: the line occupies one buffer row (NoWrap, or any 1-row item).
/// - `map_visual`: whether the caller painted `text` bidi-reordered (via
///   `set_line_safe_bidi`). Only then are match columns remapped to visual
///   cells; callers that paint logically (custom list renderers) pass `false`
///   so highlights are not shifted off the glyphs they paint.
#[allow(clippy::too_many_arguments)]
pub fn paint_match_highlights(
    buf: &mut Buffer,
    area: Rect,
    row_y: u16,
    viewport_bottom: u16,
    skip: u16,
    prefix_w: u16,
    text: &str,
    re: &regex::Regex,
    single_row: bool,
    map_visual: bool,
) {
    if text.is_empty() {
        return;
    }

    if single_row {
        let map_bidi = map_visual && is_enabled() && needs_bidi(text);
        for m in re.find_iter(text) {
            let log_start = byte_offset_to_display_col(text, m.start());
            let log_end = byte_offset_to_display_col(text, m.end());
            let ranges = if map_bidi {
                logical_cols_to_visual(text, log_start, log_end)
            } else {
                vec![(log_start, log_end)]
            };
            for (col_start, col_end) in ranges {
                for col in col_start..col_end {
                    let x = area.x + prefix_w + col as u16;
                    if x < area.x + area.width {
                        invert_cell(&mut buf[(x, row_y)]);
                    }
                }
            }
        }
        return;
    }

    let text_w = area.width.saturating_sub(prefix_w) as usize;
    let ranges = wrap_byte_ranges_matching(text, text_w);
    for m in re.find_iter(text) {
        for seg in byte_range_to_row_cols(text, &ranges, m.start()..m.end()) {
            if seg.row < skip as usize {
                continue;
            }
            let y = row_y + (seg.row - skip as usize) as u16;
            if y >= viewport_bottom {
                break;
            }
            let row_range = &ranges[seg.row];
            let row_text = &text[row_range.start..row_range.end];
            let visual_ranges = if map_visual && is_enabled() && needs_bidi(row_text) {
                logical_cols_to_visual(row_text, seg.col_start, seg.col_end)
            } else {
                vec![(seg.col_start, seg.col_end)]
            };
            for (col_start, col_end) in visual_ranges {
                for col in col_start..col_end {
                    let x = area.x + prefix_w + col as u16;
                    if x < area.x + area.width {
                        invert_cell(&mut buf[(x, y)]);
                    }
                }
            }
        }
    }
}

/// Apply the terminal's REVERSED attribute so the fg/bg swap is native and
/// respects the user's theme.
fn invert_cell(cell: &mut ratatui::buffer::Cell) {
    cell.modifier.insert(ratatui::style::Modifier::REVERSED);
}
