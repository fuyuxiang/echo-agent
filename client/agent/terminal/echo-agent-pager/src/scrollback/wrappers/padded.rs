//! Padded wrapper - adds horizontal padding around content.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;

use crate::render::Renderable;

/// Wraps content with horizontal padding.
///
/// Adds left and right padding columns. Optionally fills padding with a background.
///
/// ```text
/// │PP│  Content here...  │P│
/// │PP│  More content...  │P│
///  ↑↑                     ↑
///  Left padding (2)       Right padding (1)
/// ```
pub struct Padded<'a, T> {
    inner: &'a T,
    left: u16,
    right: u16,
    bg: Option<ratatui::style::Color>,
}

impl<'a, T> Padded<'a, T> {
    /// Create a new padded wrapper.
    pub fn new(inner: &'a T, left: u16, right: u16) -> Self {
        Self {
            inner,
            left,
            right,
            bg: None,
        }
    }

    /// Create with standard pager padding (2 left, 1 right).
    pub fn standard(inner: &'a T) -> Self {
        Self::new(inner, 2, 1)
    }

    /// Set background color for padding and content area.
    pub fn with_bg(mut self, color: ratatui::style::Color) -> Self {
        self.bg = Some(color);
        self
    }
}

impl<T: Renderable> Renderable for Padded<'_, T> {
    fn desired_height(&self, width: u16) -> u16 {
        // Padding takes left + right columns
        let content_width = width.saturating_sub(self.left + self.right);
        self.inner.desired_height(content_width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Fill background if specified
        if let Some(bg) = self.bg {
            let bg_style = Style::default().bg(bg);
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }
        }

        // Split horizontally: [left pad] [content] [right pad]
        let [_left_area, content_area, _right_area] = Layout::horizontal([
            Constraint::Length(self.left),
            Constraint::Min(0),
            Constraint::Length(self.right),
        ])
        .areas(area);

        // Render inner content
        if content_area.width > 0 {
            self.inner.render(content_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;

    /// Simple test content that renders as fixed height.
    struct TestContent {
        height: u16,
        text: &'static str,
    }

    impl Renderable for TestContent {
        fn desired_height(&self, _width: u16) -> u16 {
            self.height
        }

        fn render(&self, area: Rect, buf: &mut Buffer) {
            for y in area.y..area.y + area.height.min(self.height) {
                buf.set_string(area.x, y, self.text, Style::default());
            }
        }
    }

    #[test]
    fn test_desired_height_accounts_for_padding() {
        let content = TestContent {
            height: 3,
            text: "test",
        };
        let padded = Padded::new(&content, 2, 1);

        // Width 80 -> content gets 77 (80 - 2 - 1)
        assert_eq!(padded.desired_height(80), 3);
    }

    #[test]
    fn test_standard_padding() {
        let content = TestContent {
            height: 1,
            text: "x",
        };
        let padded = Padded::standard(&content);

        // Standard is 2 left, 1 right
        // Width 10 -> content gets 7
        assert_eq!(padded.desired_height(10), 1);
    }

    #[test]
    fn test_render_places_content_with_offset() {
        let content = TestContent {
            height: 1,
            text: "Hi",
        };
        let padded = Padded::new(&content, 2, 1);

        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        padded.render(area, &mut buf);

        // Content starts at column 2 (after 2-char left padding)
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "H");
        assert_eq!(buf.cell((3, 0)).unwrap().symbol(), "i");
    }

    #[test]
    fn test_render_with_background() {
        let content = TestContent {
            height: 1,
            text: "X",
        };
        let padded = Padded::new(&content, 1, 1).with_bg(Color::Blue);

        let area = Rect::new(0, 0, 5, 1);
        let mut buf = Buffer::empty(area);
        padded.render(area, &mut buf);

        // All cells should have blue background
        for x in 0..5 {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(cell.bg, Color::Blue);
        }
    }
}
