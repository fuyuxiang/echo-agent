//! Multiline preview overlay widget.
//!
//! Renders a bordered popup showing a preview of multiline content.
//! Shows first N and last N lines with a `⋮` separator when content
//! exceeds the preview limit.
//!
//! Used for:
//! - Paste element previews in the prompt widget
//! - Queue item previews in the queue pane

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use super::line_utils::{truncate_line, truncate_str};
use super::safe_buf::SafeBuf;

// ---------------------------------------------------------------------------
// PreviewStyle — configurable colors
// ---------------------------------------------------------------------------

/// Visual styling for the preview overlay.
#[derive(Debug, Clone, Copy)]
pub struct PreviewStyle {
    /// Background color for the entire overlay box.
    pub bg: Color,
    /// Foreground color for content text.
    pub text_fg: Color,
    /// Foreground color for the border and dots separator.
    pub border_fg: Color,
}

impl PreviewStyle {
    /// Create a style with explicit colors.
    pub fn new(bg: Color, text_fg: Color, border_fg: Color) -> Self {
        Self {
            bg,
            text_fg,
            border_fg,
        }
    }
}

// ---------------------------------------------------------------------------
// PreviewConfig — layout configuration
// ---------------------------------------------------------------------------

/// Layout configuration for the preview overlay.
#[derive(Debug, Clone)]
pub struct PreviewConfig {
    /// Number of lines to show from the top and bottom when truncating.
    /// If content has more than `preview_lines * 2` lines, shows first N,
    /// dots separator, and last N lines.
    pub preview_lines: usize,

    /// Width of the overlay as a fraction of the available width (0.0 - 1.0).
    /// Default: 0.75 (3/4 of available width).
    pub width_ratio: f32,

    /// Vertical gap between the overlay's bottom border and the anchor point.
    /// 0 = overlay sits flush against the anchor.
    pub bottom_gap: u16,

    /// Minimum width for the overlay. Below this, the overlay won't render.
    pub min_width: u16,

    /// Minimum height for the overlay area. Below this, the overlay won't render.
    pub min_height: u16,

    /// Optional one-line hint painted into the bottom border row, e.g.
    /// `╰─ enter to expand ────╯`. Costs no content row; skipped when the
    /// box is too narrow to fit readable text. `None` (the default)
    /// leaves the plain border.
    pub hint: Option<Line<'static>>,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            preview_lines: 3,
            width_ratio: 0.75,
            bottom_gap: 0,
            min_width: 20,
            min_height: 5,
            hint: None,
        }
    }
}

// ---------------------------------------------------------------------------
// render_preview_overlay — main rendering function
// ---------------------------------------------------------------------------

/// Render a multiline preview overlay.
///
/// The overlay is anchored at the bottom of `area`, showing a bordered box
/// with the content preview. If content exceeds `config.preview_lines * 2`
/// lines, shows first N lines, a `⋮ (X more lines)` separator, and last N lines.
///
/// # Arguments
///
/// * `buf` - The buffer to render into
/// * `area` - The available area for the overlay (anchored at bottom)
/// * `content` - The multiline text content to preview
/// * `style` - Visual styling (colors)
/// * `config` - Layout configuration
///
/// # Returns
///
/// The actual `Rect` where the overlay was rendered, or `None` if the overlay
/// couldn't be rendered (area too small, content empty).
pub fn render_preview_overlay(
    buf: &mut Buffer,
    area: Rect,
    content: &str,
    style: PreviewStyle,
    config: PreviewConfig,
) -> Option<Rect> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Don't render if content is empty or area is too small
    if total == 0 || area.height < config.min_height || area.width < config.min_width {
        return None;
    }

    // Calculate content layout
    let needs_dots = total > config.preview_lines * 2;
    let content_lines: usize = if needs_dots {
        config.preview_lines * 2 + 1 // top + dots + bottom
    } else {
        total
    };

    // Box dimensions: border(1) + content + border(1)
    let box_height = (content_lines as u16 + 2).min(area.height);
    let box_width = ((area.width as f32) * config.width_ratio) as u16;

    // Anchor at bottom of area
    let anchor_bottom = area.y + area.height - config.bottom_gap;
    let box_x = area.x + (area.width.saturating_sub(box_width)) / 2;
    let box_y = anchor_bottom.saturating_sub(box_height);

    let box_area = Rect {
        x: box_x,
        y: box_y,
        width: box_width,
        height: box_height,
    };

    // Build the bordered block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(style.border_fg))
        .style(Style::default().bg(style.bg));
    let inner = block.inner(box_area);

    // Clear background - fill every cell so underlying content doesn't bleed through
    Clear.render(box_area, buf);
    buf.set_style(box_area, Style::default().bg(style.bg));

    // Render the border
    block.render(box_area, buf);

    // Render content
    let text_style = Style::default().fg(style.text_fg).bg(style.bg);
    let dots_style = Style::default().fg(style.border_fg).bg(style.bg);

    render_content_lines(
        buf,
        inner,
        &lines,
        needs_dots,
        config.preview_lines,
        text_style,
        dots_style,
    );

    // Hint lives in the bottom border row: costs no content row, and the
    // border interruption reads as a label even when a theme aliases the
    // hint palette to the border/content colors.
    if let Some(hint) = &config.hint {
        render_border_hint(buf, box_area, hint, style.bg);
    }

    Some(box_area)
}

/// Render the content lines into the inner area.
fn render_content_lines(
    buf: &mut Buffer,
    inner: Rect,
    lines: &[&str],
    needs_dots: bool,
    preview_lines: usize,
    text_style: Style,
    dots_style: Style,
) {
    let total = lines.len();
    let mut row = 0u16;
    let max_rows = inner.height;

    if needs_dots {
        // Top lines
        for line in lines.iter().take(preview_lines) {
            if row >= max_rows {
                break;
            }
            render_line(buf, inner.x, inner.y + row, inner.width, line, text_style);
            row += 1;
        }

        // Dots separator
        if row < max_rows {
            let omitted = total - preview_lines * 2;
            let dots_text = format!("⋮ ({omitted} more lines)");
            buf.set_span_safe(
                inner.x,
                inner.y + row,
                &Span::styled(dots_text, dots_style),
                inner.width,
            );
            row += 1;
        }

        // Bottom lines
        let start = total.saturating_sub(preview_lines);
        for line in lines.iter().skip(start) {
            if row >= max_rows {
                break;
            }
            render_line(buf, inner.x, inner.y + row, inner.width, line, text_style);
            row += 1;
        }
    } else {
        // Show all lines
        for line in lines {
            if row >= max_rows {
                break;
            }
            render_line(buf, inner.x, inner.y + row, inner.width, line, text_style);
            row += 1;
        }
    }
}

/// Render a single truncated line.
#[inline]
fn render_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &str, style: Style) {
    let truncated = truncate_str(line, width as usize);
    buf.set_span_safe(x, y, &Span::styled(truncated, style), width);
}

/// Paint the hint into the bottom border row, left-aligned after the
/// corner and one dash, padded with a space on each side so the text
/// stands off the dashes: `╰─ enter to expand ────╯`. The corners and
/// one dash per side are never overwritten. Skipped entirely when the
/// box is too narrow for readable text.
fn render_border_hint(buf: &mut Buffer, box_area: Rect, hint: &Line<'static>, bg: Color) {
    // Chrome around the text: corners (2) + one dash each side (2) + pads (2).
    const CHROME: u16 = 6;
    // Below this the truncated text is noise — keep the plain border.
    const MIN_TEXT_WIDTH: u16 = 8;
    let text_width = box_area.width.saturating_sub(CHROME);
    if text_width < MIN_TEXT_WIDTH {
        return;
    }

    let mut line = truncate_line(hint.clone(), text_width as usize);
    // The box bg wins so the hint sits on the border row fill.
    for span in &mut line.spans {
        span.style = span.style.bg(bg);
    }
    let pad = Span::styled(" ", Style::default().bg(bg));
    let mut spans = vec![pad.clone()];
    spans.append(&mut line.spans);
    spans.push(pad);

    let y = box_area.y + box_area.height - 1;
    buf.set_line_safe(box_area.x + 2, y, &Line::from(spans), box_area.width - 4);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_style() -> PreviewStyle {
        PreviewStyle::new(
            Color::Indexed(234), // grayscale 28  — dark bg
            Color::Indexed(189), // (215,215,255) — light text
            Color::Indexed(60),  //  (95,95,135)  — dim border
        )
    }

    #[test]
    fn test_empty_content_returns_none() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 80, 20),
            "",
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_area_too_small_returns_none() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 10, 3), // below min_height=5
            "hello\nworld",
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_single_line_renders() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 10),
            "single line",
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_some());
        let rect = result.unwrap();
        // Box should be 3 rows: border + 1 content + border
        assert_eq!(rect.height, 3);
    }

    #[test]
    fn test_few_lines_no_dots() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 15));
        let content = "line1\nline2\nline3\nline4";
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 15),
            content,
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_some());
        let rect = result.unwrap();
        // 4 lines + 2 borders = 6 rows
        assert_eq!(rect.height, 6);

        // Should NOT contain dots separator (4 lines <= 6 = preview_lines * 2)
        let buf_str = buffer_to_string(&buf);
        assert!(!buf_str.contains("⋮"), "Should not have dots: {}", buf_str);
    }

    #[test]
    fn test_many_lines_shows_dots() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 15));
        let content = (1..=10)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 15),
            &content,
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_some());

        // Should contain dots separator (10 lines > 6 = preview_lines * 2)
        let buf_str = buffer_to_string(&buf);
        assert!(buf_str.contains("⋮"), "Should have dots: {}", buf_str);
        assert!(
            buf_str.contains("4 more lines"),
            "Should show omitted count: {}",
            buf_str
        );
    }

    #[test]
    fn test_custom_preview_lines() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        let content = (1..=20)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let config = PreviewConfig {
            preview_lines: 5,
            ..Default::default()
        };
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 20),
            &content,
            test_style(),
            config,
        );
        assert!(result.is_some());
        let rect = result.unwrap();
        // 5 top + 1 dots + 5 bottom + 2 borders = 13 rows
        assert_eq!(rect.height, 13);

        let buf_str = buffer_to_string(&buf);
        assert!(
            buf_str.contains("10 more lines"),
            "Should show 10 omitted: {}",
            buf_str
        );
    }

    #[test]
    fn test_width_ratio() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 10));
        let config = PreviewConfig {
            width_ratio: 0.5,
            ..Default::default()
        };
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 100, 10),
            "hello",
            test_style(),
            config,
        );
        assert!(result.is_some());
        let rect = result.unwrap();
        assert_eq!(rect.width, 50); // 100 * 0.5 = 50
    }

    #[test]
    fn test_long_line_truncated() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
        let long_line = "a".repeat(100);
        let result = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 30, 10),
            &long_line,
            test_style(),
            PreviewConfig::default(),
        );
        assert!(result.is_some());

        // Content should be truncated with ellipsis
        let buf_str = buffer_to_string(&buf);
        assert!(
            buf_str.contains("…"),
            "Long line should be truncated: {}",
            buf_str
        );
    }

    fn test_hint() -> Line<'static> {
        Line::from(vec![
            Span::styled("enter", Style::default()),
            Span::styled(" to expand", Style::default()),
        ])
    }

    /// Helper: one buffer row as a string.
    fn row_to_string(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// Assert the box's bottom border row keeps both rounded corners.
    fn assert_corners(buf: &Buffer, rect: Rect) {
        let y = rect.y + rect.height - 1;
        assert_eq!(buf[(rect.x, y)].symbol(), "╰");
        assert_eq!(buf[(rect.x + rect.width - 1, y)].symbol(), "╯");
    }

    #[test]
    fn test_hint_renders_in_bottom_border() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        let config = PreviewConfig {
            hint: Some(test_hint()),
            ..Default::default()
        };
        let rect = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 10),
            "hello\nworld",
            test_style(),
            config,
        )
        .unwrap();
        // The hint costs no row: 2 content + 2 borders.
        assert_eq!(rect.height, 4);
        assert!(row_to_string(&buf, rect.y + 1).contains("hello"));
        assert!(row_to_string(&buf, rect.y + 2).contains("world"));
        let bottom = row_to_string(&buf, rect.y + rect.height - 1);
        assert!(bottom.contains("enter to expand"), "{bottom}");
        assert_corners(&buf, rect);
    }

    #[test]
    fn test_hint_costs_no_height() {
        let area = Rect::new(0, 0, 40, 10);
        let content = "l1\nl2\nl3";
        let mut buf_hint = Buffer::empty(area);
        let config = PreviewConfig {
            hint: Some(test_hint()),
            ..Default::default()
        };
        let with_hint = render_preview_overlay(&mut buf_hint, area, content, test_style(), config);
        let mut buf_plain = Buffer::empty(area);
        let without = render_preview_overlay(
            &mut buf_plain,
            area,
            content,
            test_style(),
            PreviewConfig::default(),
        );
        assert!(with_hint.is_some());
        assert_eq!(with_hint, without, "hint must not change the box geometry");
    }

    #[test]
    fn test_hint_none_keeps_plain_border() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        let rect = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 40, 10),
            "hello\nworld",
            test_style(),
            PreviewConfig::default(),
        )
        .unwrap();
        assert_corners(&buf, rect);
        // Every cell between the corners is a border dash.
        let y = rect.y + rect.height - 1;
        for x in rect.x + 1..rect.x + rect.width - 1 {
            assert_eq!(buf[(x, y)].symbol(), "─", "col {x}");
        }
    }

    #[test]
    fn test_hint_truncated_at_narrow_width() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
        let config = PreviewConfig {
            hint: Some(Line::from("a very long hint that cannot possibly fit")),
            ..Default::default()
        };
        let rect = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 30, 10),
            "hi",
            test_style(),
            config,
        )
        .unwrap();
        let bottom = row_to_string(&buf, rect.y + rect.height - 1);
        assert!(bottom.contains("…"), "hint should ellipsize: {bottom}");
        assert!(!bottom.contains("possibly"), "{bottom}");
        assert_corners(&buf, rect);
    }

    #[test]
    fn test_hint_skipped_when_ultra_narrow() {
        // Box of 12 cells leaves 6 for text — below the readability floor,
        // so the border stays plain.
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 10));
        let config = PreviewConfig {
            hint: Some(test_hint()),
            min_width: 10,
            ..Default::default()
        };
        let rect = render_preview_overlay(
            &mut buf,
            Rect::new(0, 0, 16, 10),
            "hi",
            test_style(),
            config,
        )
        .unwrap();
        let y = rect.y + rect.height - 1;
        for x in rect.x + 1..rect.x + rect.width - 1 {
            assert_eq!(buf[(x, y)].symbol(), "─", "col {x}");
        }
        assert_corners(&buf, rect);
    }

    /// Helper: convert buffer to string for assertions.
    fn buffer_to_string(buf: &Buffer) -> String {
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }
}
