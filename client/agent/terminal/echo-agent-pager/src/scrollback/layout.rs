//! Horizontal layout for scrollback entries.
//!
//! Defines the column structure shared by all scrollback entries.

use ratatui::layout::{Constraint, Layout, Rect};

use crate::appearance::LayoutConfig;

/// Horizontal layout columns for scrollback entries.
///
/// ```text
/// │A│PL│    Content    │PR│
/// │1│ 2│     flex      │ 1│
/// ```
///
/// Where:
/// - A = Accent line (1 char)
/// - PL = Left padding (configurable, default 2)
/// - Content = Flexible width
/// - PR = Right padding (configurable, default 1)
///
/// Note: Selection borders are drawn INTO the outer viewport padding,
/// not as part of this layout. Scrollbar is handled separately.
#[derive(Debug, Clone)]
pub struct HorizontalLayout {
    /// Accent line column.
    pub accent: Rect,
    /// Left padding area (between accent and content).
    pub left_padding: Rect,
    /// Main content area.
    pub content: Rect,
    /// Right padding area.
    pub right_padding: Rect,
}

impl HorizontalLayout {
    /// Accent width is always 1.
    pub const ACCENT: u16 = 1;

    /// Create layout for the given area with config values.
    pub fn new(area: Rect, config: &LayoutConfig) -> Self {
        let [accent, left_padding, content, right_padding] = Layout::horizontal([
            Constraint::Length(Self::ACCENT),
            Constraint::Length(config.block_pad_left),
            Constraint::Min(1), // Content takes remaining space
            Constraint::Length(config.block_pad_right),
        ])
        .areas(area);

        Self {
            accent,
            left_padding,
            content,
            right_padding,
        }
    }

    /// Create layout with default config (for backwards compatibility).
    pub fn new_default(area: Rect) -> Self {
        Self::new(area, &LayoutConfig::default())
    }

    /// Total chrome width for a given config.
    pub fn chrome_width(config: &LayoutConfig) -> u16 {
        Self::ACCENT + config.block_pad_left + config.block_pad_right
    }

    /// Get the area for rendering entry content (accent through right padding).
    ///
    /// This is the area passed to `EntryRenderer`.
    /// Layout: `│A│PL│Content│PR│`
    pub fn entry_content_area(&self) -> Rect {
        Rect {
            x: self.accent.x,
            y: self.accent.y,
            width: self.accent.width
                + self.left_padding.width
                + self.content.width
                + self.right_padding.width,
            height: self.accent.height,
        }
    }

    /// Get the accent column area.
    pub fn accent_area(&self) -> Rect {
        self.accent
    }

    /// Get the content width (for BlockContext).
    pub fn content_width(&self) -> u16 {
        self.content.width
    }

    /// Get the full entry area (same as entry_content_area).
    pub fn entry_area(&self) -> Rect {
        self.entry_content_area()
    }

    /// Get the selection area (extends 1 column into outer padding on both sides).
    ///
    /// The selection border is drawn INTO the padding areas:
    /// - Left edge: 1 column before accent (in outer_hpad_left)
    /// - Right edge: 1 column after right_padding (in gap_left area before scrollbar)
    ///
    /// Returns the area where selection borders should be drawn.
    pub fn selection_area(&self) -> Rect {
        // Selection extends 1 column left of accent into outer padding
        // and 1 column right of entry into gap_left area
        let x = self.accent.x.saturating_sub(1);
        let width = self.entry_content_area().width + 2; // +1 left, +1 right

        Rect {
            x,
            y: self.accent.y,
            width,
            height: self.accent.height,
        }
    }

    /// Create a row-specific layout (same columns, different y/height).
    pub fn for_row(&self, y: u16, height: u16) -> Self {
        Self {
            accent: Rect {
                y,
                height,
                ..self.accent
            },
            left_padding: Rect {
                y,
                height,
                ..self.left_padding
            },
            content: Rect {
                y,
                height,
                ..self.content
            },
            right_padding: Rect {
                y,
                height,
                ..self.right_padding
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_horizontal_layout() {
        let config = LayoutConfig::default();
        let area = Rect::new(0, 0, 80, 10);
        let layout = HorizontalLayout::new(area, &config);

        // Check widths
        assert_eq!(layout.accent.width, 1);
        assert_eq!(layout.left_padding.width, config.block_pad_left);
        assert_eq!(layout.right_padding.width, config.block_pad_right);

        // Content should be 80 - chrome
        let chrome = HorizontalLayout::chrome_width(&config);
        assert_eq!(layout.content.width, 80 - chrome);

        // All have same height
        assert_eq!(layout.accent.height, 10);
        assert_eq!(layout.content.height, 10);
    }

    #[test]
    fn test_entry_content_area() {
        let config = LayoutConfig::default();
        let area = Rect::new(5, 10, 80, 20);
        let layout = HorizontalLayout::new(area, &config);

        let entry_area = layout.entry_content_area();

        // Entry area starts at accent column
        assert_eq!(entry_area.x, layout.accent.x);
        // Width includes accent + left_pad + content + right_pad
        assert_eq!(
            entry_area.width,
            1 + config.block_pad_left + layout.content.width + config.block_pad_right
        );
    }

    #[test]
    fn test_for_row() {
        let config = LayoutConfig::default();
        let area = Rect::new(0, 0, 80, 10);
        let layout = HorizontalLayout::new(area, &config);

        let row_layout = layout.for_row(5, 3);

        assert_eq!(row_layout.accent.y, 5);
        assert_eq!(row_layout.accent.height, 3);
        assert_eq!(row_layout.content.y, 5);
        assert_eq!(row_layout.content.height, 3);
        // X positions should be unchanged
        assert_eq!(row_layout.accent.x, layout.accent.x);
        assert_eq!(row_layout.content.x, layout.content.x);
    }

    #[test]
    fn test_selection_area() {
        let config = LayoutConfig::default();
        // Area starts at x=5 (simulating outer padding already applied)
        let area = Rect::new(5, 10, 80, 20);
        let layout = HorizontalLayout::new(area, &config);

        let selection = layout.selection_area();

        // Selection area should extend 1 column LEFT of accent into outer padding
        assert_eq!(selection.x, layout.accent.x - 1);
        // Width should be entry_content_area width + 2 (1 left, 1 right)
        assert_eq!(selection.width, layout.entry_content_area().width + 2);
        // Y and height same as accent
        assert_eq!(selection.y, layout.accent.y);
        assert_eq!(selection.height, layout.accent.height);
    }

    #[test]
    fn test_selection_area_at_edge() {
        let config = LayoutConfig::default();
        // Area starts at x=0 (no outer padding)
        let area = Rect::new(0, 0, 80, 10);
        let layout = HorizontalLayout::new(area, &config);

        let selection = layout.selection_area();

        // Selection at edge should saturate at x=0 (no underflow)
        assert_eq!(selection.x, 0);
        // Width is entry width + 2
        assert_eq!(selection.width, layout.entry_content_area().width + 2);
    }
}
