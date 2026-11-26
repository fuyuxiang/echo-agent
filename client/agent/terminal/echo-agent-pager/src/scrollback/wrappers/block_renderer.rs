//! BlockRenderer - bridges BlockContent to Renderable.
//!
//! This wrapper takes a block that implements `BlockContent` and renders
//! its output into a given area, implementing the `Renderable` trait.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::appearance::AppearanceConfig;
use crate::render::{Renderable, SafeBuf};
use crate::scrollback::block::BlockContent;
use crate::scrollback::types::{BlockBackground, BlockContext, DisplayMode};
use crate::theme::Theme;

/// Renders a `BlockContent` implementation as a `Renderable`.
pub struct BlockRenderer<'a, B> {
    block: &'a B,
    mode: DisplayMode,
    is_running: bool,
    raw: bool,
    background: Option<ratatui::style::Color>,
    max_lines: Option<u16>,
    appearance: AppearanceConfig,
}

impl<'a, B> BlockRenderer<'a, B> {
    pub fn new(block: &'a B) -> Self {
        Self {
            block,
            mode: DisplayMode::Expanded,
            is_running: false,
            raw: false,
            background: None,
            max_lines: None,
            appearance: AppearanceConfig::default(),
        }
    }

    pub fn mode(mut self, mode: DisplayMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn running(mut self, is_running: bool) -> Self {
        self.is_running = is_running;
        self
    }

    pub fn raw(mut self, raw: bool) -> Self {
        self.raw = raw;
        self
    }

    pub fn background(mut self, bg: ratatui::style::Color) -> Self {
        self.background = Some(bg);
        self
    }

    pub fn max_lines(mut self, max: u16) -> Self {
        self.max_lines = Some(max);
        self
    }

    pub fn appearance(mut self, appearance: AppearanceConfig) -> Self {
        self.appearance = appearance;
        self
    }
}

impl<B: BlockContent> BlockRenderer<'_, B> {
    fn make_context(&self, width: u16) -> BlockContext {
        BlockContext {
            mode: self.mode,
            is_running: self.is_running,
            width,
            raw: self.raw,
            max_lines: self.max_lines,
            appearance: self.appearance.clone(),
            is_selected: false,
            cwd: None,
        }
    }

    fn resolve_background(&self, block_bg: BlockBackground) -> Option<ratatui::style::Color> {
        // Explicit override takes precedence
        if let Some(bg) = self.background {
            return Some(bg);
        }

        // Otherwise use block's declared background
        let theme = Theme::current();
        match block_bg {
            BlockBackground::None => None,
            BlockBackground::Light => Some(theme.bg_light),
            BlockBackground::Dark => Some(theme.bg_dark),
        }
    }
}

impl<B: BlockContent> Renderable for BlockRenderer<'_, B> {
    fn desired_height(&self, width: u16) -> u16 {
        let ctx = self.make_context(width);
        let output = self.block.output(&ctx);
        let has_vpad = self.block.has_vpad(&ctx);

        let content_height = output.len() as u16;
        let vpad = if has_vpad { 2 } else { 0 }; // top + bottom

        content_height + vpad
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let ctx = self.make_context(area.width);
        let output = self.block.output(&ctx);
        let has_vpad = self.block.has_vpad(&ctx);
        let block_bg = self.block.background(&ctx);

        // Resolve background color
        let bg_color = self.resolve_background(block_bg);

        // Fill background if specified
        if let Some(bg) = bg_color {
            let bg_style = Style::default().bg(bg);
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }
        }

        let mut row = area.y;
        let max_row = area.y + area.height;

        // Top vpad (empty row)
        if has_vpad && row < max_row {
            row += 1;
        }

        // Content lines
        for line in &output.lines {
            if row >= max_row {
                break;
            }

            // Apply line-specific background if set
            // Respects bg_start_col for partial background
            if let Some(line_bg) = line.background {
                let bg_x = area.x + line.bg_start_col;
                let bg_width = area.width.saturating_sub(line.bg_start_col);
                if bg_width > 0 {
                    let line_rect = Rect::new(bg_x, row, bg_width, 1);
                    buf.set_style(line_rect, Style::default().bg(line_bg));
                }
            }

            // Content paint (bidi-aware when rtl_bidi is on).
            buf.set_line_safe_bidi(area.x, row, &line.content, area.width);
            row += 1;
        }

        // Bottom vpad (empty row) - just skip, background already applied
        // (no explicit rendering needed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::block::StubBlock;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;

    #[test]
    fn test_desired_height_with_vpad() {
        let block = StubBlock::new("Hello", Color::Blue);
        let renderer = BlockRenderer::new(&block);

        // StubBlock has 1 line + vpad (top + bottom) = 3
        assert_eq!(renderer.desired_height(80), 3);
    }

    #[test]
    fn test_render_fills_area() {
        let block = StubBlock::new("Test content", Color::Blue);
        let renderer = BlockRenderer::new(&block);

        let area = Rect::new(0, 0, 20, 5);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // Row 0 = top vpad (empty)
        // Row 1 = content "Test content"
        // Row 2 = bottom vpad (empty)
        // Content should be at row 1
        let content_cell = buf.cell((0, 1)).unwrap();
        assert_eq!(content_cell.symbol(), "T");
    }

    #[test]
    fn test_render_with_explicit_background() {
        let block = StubBlock::new("BG test", Color::Blue);
        let renderer = BlockRenderer::new(&block).background(Color::Red);

        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        renderer.render(area, &mut buf);

        // All cells should have red background
        for y in 0..3 {
            for x in 0..10 {
                assert_eq!(buf.cell((x, y)).unwrap().bg, Color::Red);
            }
        }
    }

    #[test]
    fn test_different_display_modes() {
        let block = StubBlock::new("Mode test", Color::Blue);

        let expanded = BlockRenderer::new(&block).mode(DisplayMode::Expanded);
        let collapsed = BlockRenderer::new(&block).mode(DisplayMode::Collapsed);

        // Both should have same height for StubBlock (it doesn't vary by mode)
        assert_eq!(expanded.desired_height(80), collapsed.desired_height(80));
    }
}
