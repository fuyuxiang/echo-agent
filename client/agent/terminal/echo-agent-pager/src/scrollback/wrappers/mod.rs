//! Wrapper types for composable rendering.
//!
//! These wrappers implement the `Renderable` trait and add decorations
//! (accent lines, padding, etc.) around inner content.
//!
//! ## Composition Example
//!
//! ```text
//! Padded::standard(                    // Adds left=2, right=1 padding
//!     &Accented::with_fg(              // Adds accent line on left
//!         &BlockRenderer::new(&block), // Renders block content
//!         Color::Blue,
//!     )
//! )
//! ```
//!
//! This produces:
//! ```text
//! │PP│A│ Content here... │P│
//!  ↑↑  ↑                   ↑
//!  │   └─ Accent           └─ Right padding
//!  └───── Left padding
//! ```

mod accented;
mod block_renderer;
mod entry_renderer;
mod padded;

pub use accented::Accented;
pub use block_renderer::BlockRenderer;
pub use entry_renderer::EntryRenderer;
pub(crate) use entry_renderer::group_header_chrome_prefix_width;
pub use padded::Padded;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Renderable;
    use crate::scrollback::block::StubBlock;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    /// Test that wrappers can be composed together.
    #[test]
    fn test_wrapper_composition() {
        let block = StubBlock::new("Hello", Color::Blue);
        let renderer = BlockRenderer::new(&block);
        let accented = Accented::with_fg(&renderer, Color::Blue);
        let padded = Padded::standard(&accented);

        // Check height calculation chains correctly
        // BlockRenderer: 3 (1 content + 2 vpad)
        // Accented: takes 1 column, height unchanged
        // Padded: takes 3 columns (2+1), height unchanged
        assert_eq!(padded.desired_height(80), 3);

        // Render and verify structure
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        padded.render(area, &mut buf);

        // Layout should be:
        // Cols 0-1: padding (empty)
        // Col 2: accent line
        // Cols 3+: content
        // Last col: right padding

        // Check accent is at column 2
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "┃");
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "┃");
        assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "┃");

        // Check content starts at column 3 (after accent)
        // Row 0 is vpad (empty), row 1 is content
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "H");
    }
}
