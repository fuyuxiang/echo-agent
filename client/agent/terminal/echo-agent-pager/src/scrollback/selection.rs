//! Selection box rendering for v3 pager.
//!
//! The `SelectionBox` is computed by components (like ScrollbackPane) and rendered
//! by the frame, allowing selection boxes to span component boundaries.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::render::osc8::LinkOverlay;
use crate::scrollback::text_selection::ResolvedSelectionModel;
use crate::theme::Theme;

/// Box drawing characters for selection border.
mod border_chars {
    pub const TOP_LEFT: char = '┌';
    pub const TOP_RIGHT: char = '┐';
    pub const BOTTOM_LEFT: char = '└';
    pub const BOTTOM_RIGHT: char = '┘';
    pub const VERTICAL: char = '│';
    /// Dashed vertical - used on edge rows when clipped to indicate continuation.
    pub const VERTICAL_DASHED: char = '┆';
}

/// A selection box that can be drawn around a selected block.
///
/// The box consists of:
/// - Side borders (│) on the left and right edges of `inner_area`
/// - Top corners (┌┐) one row above `inner_area` (if `!top_clipped`)
/// - Bottom corners (└┘) one row below `inner_area` (if `!bottom_clipped`)
///
/// This struct is returned by components (like ScrollbackPane) and rendered
/// by the frame, allowing selection boxes to span component boundaries.
#[derive(Debug, Clone)]
pub struct SelectionBox {
    /// The inner area surrounded by the selection border.
    pub inner_area: Rect,
    /// True if the block has rows clipped at top (scrolled out of view).
    pub top_clipped: bool,
    /// True if the block has rows clipped at bottom.
    pub bottom_clipped: bool,
    /// Style for the border (typically just fg color).
    pub style: Style,
    /// Whether to render a close control replacing the top-right corner.
    pub closable: bool,
    /// Whether the close control is currently hovered.
    pub close_hovered: bool,
    /// Optional close label; `None` uses default `✗`.
    pub close_label: Option<&'static str>,
}

/// Output from render that needs post-processing.
///
/// Render returns this instead of mutating state, keeping render pure.
/// The caller is responsible for rendering these elements after the main pass.
///
/// # Example
/// ```ignore
/// let output = pane.render_with_scratch(area, buf, &state, &mut scratch);
///
/// // Post-render pass
/// if let Some(sel) = output.selection_box {
///     sel.render(buf);
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct RenderOutput {
    /// Selection box to render around the selected entry.
    /// Rendered after main content so it can span component boundaries.
    pub selection_box: Option<SelectionBox>,
    /// Scroll info for scrollbar rendering.
    /// Viewport uses this to render the scrollbar at the correct position.
    pub scroll_info: Option<ScrollInfo>,
    /// Screen area of the individual selected entry (within a group).
    /// Used by agent_view to position inline buttons on the correct row.
    pub selected_entry_area: Option<Rect>,
    /// Per-frame resolved selection metadata for visible content.
    pub selection_model: ResolvedSelectionModel,
    /// OSC 8 link overlay for post-flush emission.
    pub link_overlay: LinkOverlay,
    /// Inline media to render via post-flush escape sequences.
    pub inline_media: Vec<crate::scrollback::render::InlineMediaPlacement>,
    /// Mermaid diagram affordance rows to paint + register click hit-rects for.
    pub diagram_affordances: Vec<crate::scrollback::render::DiagramAffordancePlacement>,
    /// Screen row (relative to the scrollback area top) of the sticky
    /// header's gap row, when this frame drew a pinned header. The ▲
    /// response-top indicator renders here; publishing the row the pane
    /// actually used keeps the indicator from re-deriving (and possibly
    /// disagreeing with) the frame's layout.
    pub sticky_gap_row: Option<u16>,
}

/// Scroll information for scrollbar rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollInfo {
    /// Current scroll offset (lines from top). `usize`: tall sessions exceed
    /// `u16::MAX`.
    pub scroll_offset: usize,
    /// Visible viewport height (lines). Stays `u16` (a terminal is never that tall).
    pub viewport_height: u16,
    /// Total content height (lines). `usize` for the same reason as `scroll_offset`.
    pub total_height: usize,
}

impl RenderOutput {
    /// Create empty render output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create render output with a selection box.
    pub fn with_selection_box(selection_box: SelectionBox) -> Self {
        Self {
            selection_box: Some(selection_box),
            ..Self::default()
        }
    }

    /// Add scroll info to the output.
    pub fn with_scroll_info(mut self, scroll_info: ScrollInfo) -> Self {
        self.scroll_info = Some(scroll_info);
        self
    }
}

impl SelectionBox {
    /// Create a new selection box with the given inner area and style.
    pub fn new(inner_area: Rect, style: Style) -> Self {
        Self {
            inner_area,
            top_clipped: false,
            bottom_clipped: false,
            style,
            closable: false,
            close_hovered: false,
            close_label: None,
        }
    }

    /// Set whether the top is clipped (no top corners).
    pub fn with_top_clipped(mut self, clipped: bool) -> Self {
        self.top_clipped = clipped;
        self
    }

    /// Set whether the bottom is clipped (no bottom corners).
    pub fn with_bottom_clipped(mut self, clipped: bool) -> Self {
        self.bottom_clipped = clipped;
        self
    }

    /// Enable a close control replacing the top-right corner `┐` (default: `✗`).
    ///
    /// Normal state: same color as the border. Hovered: bright white.
    pub fn with_closable(mut self, closable: bool, hovered: bool) -> Self {
        self.closable = closable;
        self.close_hovered = hovered;
        self
    }

    /// Set close control label (`Some` implies closable).
    pub fn with_close_label(mut self, label: Option<&'static str>) -> Self {
        self.close_label = label;
        if label.is_some() {
            self.closable = true;
        }
        self
    }

    /// Hit-test rect for the close control, if it would be rendered.
    ///
    /// Pure computation — does not touch the buffer. Use for mouse hit-testing.
    /// Returns `None` if not closable, top is clipped, or no room.
    pub fn close_button_rect(&self) -> Option<Rect> {
        if !self.closable || self.top_clipped || self.inner_area.y == 0 {
            return None;
        }
        let label_w = self
            .close_label
            .map(|s| s.chars().count() as u16)
            .unwrap_or(1)
            .max(1);
        let right_x = self.inner_area.x + self.inner_area.width.saturating_sub(1);
        let x = right_x.saturating_sub(label_w.saturating_sub(1));
        Some(Rect {
            x,
            y: self.inner_area.y - 1,
            width: label_w,
            height: 1,
        })
    }

    /// Render the selection box to the buffer.
    ///
    /// Draws:
    /// - Side borders (│) on left and right edges of inner_area
    /// - Dashed borders (┆) on edge rows when clipped, to indicate continuation
    /// - Top corners (┌┐) at inner_area.y - 1 if !top_clipped and y > 0
    /// - Bottom corners (└┘) at inner_area.y + height if !bottom_clipped
    /// - Close button (✗) left of ┐ if enabled
    pub fn render(&self, buf: &mut Buffer) {
        let area = self.inner_area;
        if area.width == 0 || area.height == 0 {
            return;
        }

        let left_x = area.x;
        let right_x = area.x + area.width.saturating_sub(1);
        let y_top = area.y;
        let y_bottom = area.y + area.height.saturating_sub(1);

        // Draw side borders
        for y in y_top..=y_bottom {
            let is_first_row = y == y_top;
            let is_last_row = y == y_bottom;
            let use_dashed =
                (is_first_row && self.top_clipped) || (is_last_row && self.bottom_clipped);

            let vert_char = if use_dashed {
                border_chars::VERTICAL_DASHED
            } else {
                border_chars::VERTICAL
            };

            if let Some(cell) = buf.cell_mut((left_x, y)) {
                cell.set_char(vert_char).set_style(self.style);
            }
            if let Some(cell) = buf.cell_mut((right_x, y)) {
                cell.set_char(vert_char).set_style(self.style);
            }
        }

        // Draw top corners (if not clipped and there's room)
        if !self.top_clipped && y_top > 0 {
            let corner_y = y_top - 1;
            if let Some(cell) = buf.cell_mut((left_x, corner_y)) {
                cell.set_char(border_chars::TOP_LEFT).set_style(self.style);
            }
            // Close control replaces ┐, or draw normal corner
            if let Some(close_rect) = self.close_button_rect() {
                let style = if self.close_hovered {
                    Style::default().fg(Theme::current().text_primary)
                } else {
                    self.style
                };
                if let Some(label) = self.close_label {
                    use crate::render::SafeBuf;
                    buf.set_string_safe(close_rect.x, close_rect.y, label, style);
                } else if let Some(cell) = buf.cell_mut((close_rect.x, close_rect.y)) {
                    cell.set_symbol(crate::glyphs::ballot_x()).set_style(style);
                }
            } else if let Some(cell) = buf.cell_mut((right_x, corner_y)) {
                cell.set_char(border_chars::TOP_RIGHT).set_style(self.style);
            }
        }

        // Draw bottom corners (if not clipped)
        if !self.bottom_clipped {
            let corner_y = y_bottom + 1;
            if let Some(cell) = buf.cell_mut((left_x, corner_y)) {
                cell.set_char(border_chars::BOTTOM_LEFT)
                    .set_style(self.style);
            }
            if let Some(cell) = buf.cell_mut((right_x, corner_y)) {
                cell.set_char(border_chars::BOTTOM_RIGHT)
                    .set_style(self.style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selection_box_render() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection = SelectionBox::new(Rect::new(0, 2, 10, 4), Style::default());

        selection.render(&mut buf);

        // Check top corners at y=1 (inner_area.y - 1)
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "┌");
        assert_eq!(buf.cell((9, 1)).unwrap().symbol(), "┐");

        // Check side borders at y=2..=5 (all solid, not clipped)
        for y in 2..=5 {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "│");
            assert_eq!(buf.cell((9, y)).unwrap().symbol(), "│");
        }

        // Check bottom corners at y=6 (inner_area.y + height)
        assert_eq!(buf.cell((0, 6)).unwrap().symbol(), "└");
        assert_eq!(buf.cell((9, 6)).unwrap().symbol(), "┘");
    }

    #[test]
    fn test_selection_box_top_clipped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection =
            SelectionBox::new(Rect::new(0, 2, 10, 4), Style::default()).with_top_clipped(true);

        selection.render(&mut buf);

        // Top corners should NOT be drawn
        assert_ne!(buf.cell((0, 1)).unwrap().symbol(), "┌");
        assert_ne!(buf.cell((9, 1)).unwrap().symbol(), "┐");

        // First row (y=2) should have DASHED borders
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 2)).unwrap().symbol(), "┆");

        // Middle rows should have solid borders
        for y in 3..=5 {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "│");
            assert_eq!(buf.cell((9, y)).unwrap().symbol(), "│");
        }

        // Bottom corners should be drawn
        assert_eq!(buf.cell((0, 6)).unwrap().symbol(), "└");
    }

    #[test]
    fn test_selection_box_bottom_clipped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection =
            SelectionBox::new(Rect::new(0, 2, 10, 4), Style::default()).with_bottom_clipped(true);

        selection.render(&mut buf);

        // Top corners should be drawn
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "┌");
        assert_eq!(buf.cell((9, 1)).unwrap().symbol(), "┐");

        // First rows should have solid borders
        for y in 2..=4 {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "│");
            assert_eq!(buf.cell((9, y)).unwrap().symbol(), "│");
        }

        // Last row (y=5) should have DASHED borders
        assert_eq!(buf.cell((0, 5)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 5)).unwrap().symbol(), "┆");

        // Bottom corners should NOT be drawn
        assert_ne!(buf.cell((0, 6)).unwrap().symbol(), "└");
        assert_ne!(buf.cell((9, 6)).unwrap().symbol(), "┘");
    }

    #[test]
    fn test_selection_box_both_clipped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection = SelectionBox::new(Rect::new(0, 2, 10, 4), Style::default())
            .with_top_clipped(true)
            .with_bottom_clipped(true);

        selection.render(&mut buf);

        // No corners should be drawn
        assert_ne!(buf.cell((0, 1)).unwrap().symbol(), "┌");
        assert_ne!(buf.cell((0, 6)).unwrap().symbol(), "└");

        // First row (y=2) should have DASHED borders
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 2)).unwrap().symbol(), "┆");

        // Middle rows should have solid borders
        for y in 3..=4 {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "│");
            assert_eq!(buf.cell((9, y)).unwrap().symbol(), "│");
        }

        // Last row (y=5) should have DASHED borders
        assert_eq!(buf.cell((0, 5)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 5)).unwrap().symbol(), "┆");
    }

    #[test]
    fn test_selection_box_single_row_both_clipped() {
        // Edge case: only 1 row visible, both ends clipped
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection = SelectionBox::new(Rect::new(0, 3, 10, 1), Style::default())
            .with_top_clipped(true)
            .with_bottom_clipped(true);

        selection.render(&mut buf);

        // The single row should have DASHED borders (first row = last row, both clipped)
        assert_eq!(buf.cell((0, 3)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 3)).unwrap().symbol(), "┆");

        // No corners
        assert_ne!(buf.cell((0, 2)).unwrap().symbol(), "┌");
        assert_ne!(buf.cell((0, 4)).unwrap().symbol(), "└");
    }

    #[test]
    fn test_selection_box_single_row_top_clipped_only() {
        // Edge case: only 1 row visible, only top clipped
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        let selection =
            SelectionBox::new(Rect::new(0, 3, 10, 1), Style::default()).with_top_clipped(true);

        selection.render(&mut buf);

        // The single row should have DASHED borders (it's first row and top_clipped)
        assert_eq!(buf.cell((0, 3)).unwrap().symbol(), "┆");
        assert_eq!(buf.cell((9, 3)).unwrap().symbol(), "┆");

        // Bottom corners should be drawn
        assert_eq!(buf.cell((0, 4)).unwrap().symbol(), "└");
        assert_eq!(buf.cell((9, 4)).unwrap().symbol(), "┘");
    }

    #[test]
    fn test_selection_box_at_top_edge() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));

        // Selection at y=0 (no room for top corners even if not clipped)
        let selection = SelectionBox::new(Rect::new(0, 0, 10, 4), Style::default());

        selection.render(&mut buf);

        // Side borders at y=0..=3 (all solid, not clipped)
        for y in 0..=3 {
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), "│");
        }

        // Bottom corners at y=4
        assert_eq!(buf.cell((0, 4)).unwrap().symbol(), "└");
    }
}
