//! Accented wrapper - adds an accent line on the left.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;

use crate::render::Renderable;

/// Wraps content with an accent line on the left.
///
/// The accent line takes 1 column. Content renders in the remaining space.
///
/// ```text
/// │A│  Content here...  │
/// │A│  More content...  │
///  ↑
///  Accent column (1 char)
/// ```
pub struct Accented<'a, T> {
    inner: &'a T,
    style: Style,
}

impl<'a, T> Accented<'a, T> {
    /// Create a new accented wrapper.
    pub fn new(inner: &'a T, style: Style) -> Self {
        Self { inner, style }
    }

    /// Create with just a foreground color.
    pub fn with_fg(inner: &'a T, color: ratatui::style::Color) -> Self {
        Self {
            inner,
            style: Style::default().fg(color),
        }
    }
}

impl<T: Renderable> Renderable for Accented<'_, T> {
    fn desired_height(&self, width: u16) -> u16 {
        // Accent takes 1 column, so content gets width - 1
        let content_width = width.saturating_sub(1);
        self.inner.desired_height(content_width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Split horizontally: [accent (1)] [content (rest)]
        let [accent_area, content_area] =
            Layout::horizontal([Constraint::Length(1), Constraint::Min(0)]).areas(area);

        // Draw accent line for all rows
        for y in accent_area.y..accent_area.y + accent_area.height {
            buf.set_string(accent_area.x, y, crate::glyphs::accent_bar(), self.style);
        }

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
    fn test_desired_height_accounts_for_accent() {
        let content = TestContent {
            height: 3,
            text: "test",
        };
        let accented = Accented::with_fg(&content, Color::Blue);

        // Width 80 -> content gets 79
        assert_eq!(accented.desired_height(80), 3);
    }

    #[test]
    fn test_render_places_accent() {
        let content = TestContent {
            height: 2,
            text: "Hi",
        };
        let accented = Accented::with_fg(&content, Color::Blue);

        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        accented.render(area, &mut buf);

        // Check accent line is in column 0
        assert_eq!(
            buf.cell((0, 0)).unwrap().symbol(),
            crate::glyphs::accent_bar()
        );
        assert_eq!(
            buf.cell((0, 1)).unwrap().symbol(),
            crate::glyphs::accent_bar()
        );

        // Check content starts at column 1
        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "H");
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "i");
    }

    #[test]
    fn test_render_empty_area() {
        let content = TestContent {
            height: 1,
            text: "x",
        };
        let accented = Accented::with_fg(&content, Color::Blue);

        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        accented.render(area, &mut buf); // Should not panic
    }
}
