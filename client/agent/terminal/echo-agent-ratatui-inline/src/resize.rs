use std::io::{self, Write as _};

use crossterm::{cursor::MoveTo, queue, style::Print};
use ratatui::layout::Rect;

use crate::{common::TerminalLike, segment::split_into_line_segments};

/// Handles terminal resize by completely re-rendering the scrollback history.
///
/// This function uses a "nuclear option" approach: it sends RIS (Reset to Initial State)
/// to clear the entire terminal, then re-outputs all scrollback history and positions
/// the viewport appropriately.
///
/// # Why this approach?
///
/// When the terminal is resized, text reflow happens automatically *before* our application
/// receives the resize signal (SIGWINCH). This creates several problems:
///
/// 1. **Scrollback corruption**: The built-in `terminal.autoresize()` doesn't handle reflowed
///    content properly, often damaging scrollback history or leaving visual artifacts.
///
/// 2. **Viewport artifacts**: The old viewport borders get reflowed along with regular text,
///    appearing as garbage above the new viewport position. While we could try to move the
///    viewport up to avoid this, it becomes impossible when the viewport is already near the top.
///
/// 3. **Unpredictable reflow**: Different terminals handle text reflow differently, making it
///    nearly impossible to predict exactly where content will end up after resize. We tried
///    calculating reflow based on character counts, but edge cases and terminal-specific
///    behaviors made this unreliable.
///
/// The RIS + re-render approach is more drastic but provides consistency across all terminals
/// and resize scenarios. It's especially important for horizontal resizing where text reflow
/// is most problematic.
///
/// # Arguments
///
/// * `terminal` - The terminal instance to resize
/// * `history` - The complete scrollback history (with CRLF line endings)
///
/// # Returns
///
/// Returns `Ok(())` on success, or an I/O error if terminal operations fail.
pub fn resize_purge_rerender<T: TerminalLike>(terminal: &mut T, history: &str) -> io::Result<()> {
    let viewport = terminal.viewport_area();
    let size = terminal.size()?;

    // Clear current screen, clear scrollbackhistory and move the cursor to the top left corner
    // note: we could've also used RIS (\x1bc) hard reset, but it doesn't clear scrollback in iterm/terminal.app
    terminal.writer_mut().write_all(b"\x1b[2J\x1b[3J\x1b[H")?;
    terminal.writer_mut().flush()?;

    // Count newlines in history as a quick check for whether we have enough content
    // The +1 accounts for content on the first line (before any newlines)
    let num_newlines = 1 + history
        .as_bytes()
        .iter()
        .filter(|&&c| c == b'\n')
        .take(size.height.into()) // Only count up to screen height for efficiency
        .count() as u16;

    // Re-output the entire scrollback history
    queue!(terminal.writer_mut(), Print(history))?;

    // Add blank lines to reserve space for the viewport
    for _ in 0..viewport.height {
        queue!(terminal.writer_mut(), Print("\r\n"))?;
    }

    // Calculate where to position the viewport
    let viewport_y = if num_newlines + viewport.height >= size.height {
        // We have enough content to fill the screen, viewport goes at the bottom
        size.height.saturating_sub(viewport.height)
    } else {
        // Not enough content to fill the screen, need to calculate exact position
        // Use split_into_line_segments to account for line wrapping
        let segments = split_into_line_segments(history, size.width.into());
        let num_visible_lines = segments.len().min(u16::MAX as _) as u16;

        // Position viewport right after the content, but not beyond screen bottom
        num_visible_lines.min(size.height.saturating_sub(viewport.height))
    };

    // Flush all queued commands
    terminal.writer_mut().flush()?;

    // Resize and clear the viewport
    terminal.set_viewport_area(ratatui::layout::Rect {
        x: 0,
        y: viewport_y,
        width: size.width,
        height: viewport.height,
    });
    terminal.clear()?;

    Ok(())
}

/// Resize the viewport to a new height with terminal dimensions being the same.
///
/// When shrinking: Always anchors to top (gap appears at bottom)
/// When growing: Tries to expand down first, then pushes content up if needed
pub fn resize_viewport_height<T: TerminalLike>(
    terminal: &mut T,
    new_height: u16,
) -> io::Result<()> {
    macro_rules! queue {
        ($($command:expr),* $(,)?) => {{
            $(crossterm::queue!(terminal.writer_mut(), $command)?;)*
            Ok::<(), io::Error>(())
        }};
    }

    let size = terminal.size()?;
    let current_viewport = terminal.viewport_area();
    let old_height = current_viewport.height;

    if new_height == old_height {
        return Ok(());
    }

    // Ensure new height is valid
    if new_height == 0 || new_height >= size.height {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Invalid viewport height: {} (terminal height: {})",
                new_height, size.height
            ),
        ));
    }

    if new_height > old_height {
        // Growing: Smart expansion - try to expand down first, then push content up if needed
        let growth = new_height - old_height;
        let bottom_edge = current_viewport.y + current_viewport.height;
        let space_below = size.height.saturating_sub(bottom_edge);

        // Calculate the new y position
        let new_y = if space_below >= growth {
            // We have enough space below - expand down, keep same y
            current_viewport.y
        } else {
            // Need to push content up
            // Either use all space below and push up the rest, or anchor to bottom
            if space_below > 0 {
                // Use available space below and push up for the remainder
                current_viewport.y.saturating_sub(growth - space_below)
            } else {
                // Already at bottom, push everything up
                size.height.saturating_sub(new_height)
            }
        };

        // If we need to scroll content up
        if new_y < current_viewport.y {
            let scroll_amount = current_viewport.y - new_y;

            // Move to bottom and emit newlines to push content into scrollback
            queue!(MoveTo(0, size.height - 1))?;
            for _ in 0..scroll_amount {
                queue!(Print("\r\n"))?;
            }
            terminal.writer_mut().flush()?;
        }

        // Clear the old viewport
        terminal.clear()?;

        // Set the new viewport area
        terminal.set_viewport_area(Rect::new(0, new_y, current_viewport.width, new_height));
    } else {
        // Shrinking: Always anchor to top (gap appears at bottom)
        terminal.clear()?;
        terminal.set_viewport_area(Rect::new(
            0,
            current_viewport.y,
            current_viewport.width,
            new_height,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::tests::MockTerminal;

    use super::*;

    #[test]
    fn test_viewport_resize_shrink() {
        let mut terminal = MockTerminal::new(80, 25, 5);
        let original_y = terminal.viewport_area.y; // Should be 20 (25-5)

        // Shrink viewport from 5 to 3 (always anchors at top)
        resize_viewport_height(&mut terminal, 3).unwrap();

        // Check viewport was updated - y should stay the same
        assert_eq!(terminal.viewport_area.height, 3);
        assert_eq!(terminal.viewport_area.y, original_y); // Should still be 20

        // Should have cleared once
        assert_eq!(terminal.clear_count, 1);
    }

    #[test]
    fn test_viewport_resize_smart_expand() {
        let mut terminal = MockTerminal::new(80, 25, 3);

        // Start at position 20 (not at bottom)
        terminal.viewport_area.y = 20;

        // Expand viewport from 3 to 5 - should expand downward first
        resize_viewport_height(&mut terminal, 5).unwrap();

        // Check that it expanded down (kept same y)
        assert_eq!(terminal.viewport_area.height, 5);
        assert_eq!(terminal.viewport_area.y, 20); // Should stay at 20
        assert_eq!(terminal.clear_count, 1);

        // Now expand more - should hit bottom and push content up
        resize_viewport_height(&mut terminal, 6).unwrap();
        assert_eq!(terminal.viewport_area.height, 6);
        assert_eq!(terminal.viewport_area.y, 19); // Should move up to 19
        assert_eq!(terminal.clear_count, 2);
    }

    #[test]
    fn test_viewport_resize_invalid() {
        let mut terminal = MockTerminal::new(80, 25, 3);

        // Try invalid heights
        assert!(resize_viewport_height(&mut terminal, 0).is_err());
        assert!(resize_viewport_height(&mut terminal, 25).is_err());
        assert!(resize_viewport_height(&mut terminal, 26).is_err());

        // Valid edge cases
        assert!(resize_viewport_height(&mut terminal, 1).is_ok());
        assert!(resize_viewport_height(&mut terminal, 24).is_ok());
    }

    #[test]
    fn test_viewport_resize_no_op() {
        let mut terminal = MockTerminal::new(80, 25, 3);

        // Resize to same height
        resize_viewport_height(&mut terminal, 3).unwrap();

        // Should not have cleared
        assert_eq!(terminal.clear_count, 0);
        assert_eq!(terminal.viewport_area.height, 3);
    }

    #[test]
    fn test_resize_purge_rerender_empty_history() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        terminal.viewport_area.y = 22; // Bottom position

        // Test with empty history
        resize_purge_rerender(&mut terminal, "").unwrap();

        // Viewport should be at top since there's no content
        assert_eq!(terminal.viewport_area.y, 0);
        assert_eq!(terminal.viewport_area.height, 3);
        assert_eq!(terminal.clear_count, 1);
    }

    #[test]
    fn test_resize_purge_rerender_small_history() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        terminal.viewport_area.y = 22; // Bottom position

        // Test with small history (just a few lines)
        let history = "Line 1\r\nLine 2\r\nLine 3\r\n";
        resize_purge_rerender(&mut terminal, history).unwrap();

        // split_into_line_segments will count this as 3 segments (one per line)
        // So viewport should be positioned at y=3
        assert_eq!(terminal.viewport_area.y, 3);
        assert_eq!(terminal.viewport_area.height, 3);
        assert_eq!(terminal.clear_count, 1);
    }

    #[test]
    fn test_resize_purge_rerender_full_screen_history() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        terminal.viewport_area.y = 22; // Bottom position

        // Create history with more lines than screen height
        let mut history = String::new();
        for i in 1..=30 {
            history.push_str(&format!("Line {}\r\n", i));
        }

        resize_purge_rerender(&mut terminal, &history).unwrap();

        // With full screen of content, viewport should be at bottom
        assert_eq!(terminal.viewport_area.y, 25 - 3); // screen_height - viewport_height
        assert_eq!(terminal.viewport_area.height, 3);
        assert_eq!(terminal.clear_count, 1);
    }

    #[test]
    fn test_resize_purge_rerender_with_wrapped_lines() {
        let mut terminal = MockTerminal::new(40, 10, 2); // Narrow terminal
        terminal.viewport_area.y = 8;

        // Create a line that will wrap
        let long_line = "A".repeat(100); // Will wrap to ~3 lines on 40-column terminal
        let history = format!("{}\r\nShort line\r\n", long_line);

        resize_purge_rerender(&mut terminal, &history).unwrap();

        // The actual position depends on split_into_line_segments calculation
        // But it should position the viewport appropriately
        assert!(terminal.viewport_area.y <= 10 - 2);
        assert_eq!(terminal.viewport_area.height, 2);
        assert_eq!(terminal.clear_count, 1);
    }

    #[test]
    fn test_resize_purge_rerender_preserves_viewport_dimensions() {
        let mut terminal = MockTerminal::new(100, 30, 5);
        let original_width = terminal.viewport_area.width;
        let original_height = terminal.viewport_area.height;

        let history = "Some content\r\n";
        resize_purge_rerender(&mut terminal, history).unwrap();

        // Width and height should be preserved, only y position changes
        assert_eq!(terminal.viewport_area.width, original_width);
        assert_eq!(terminal.viewport_area.height, original_height);
    }

    #[test]
    fn test_resize_purge_rerender_captures_output() {
        let mut terminal = MockTerminal::new(80, 25, 3);

        let history = "Test line\r\n";
        resize_purge_rerender(&mut terminal, history).unwrap();

        // Verify RIS command was sent to writer (not real stdout)
        let output = String::from_utf8_lossy(&terminal.writer.buffer);
        assert!(
            output.contains("\x1b[2J\x1b[3J\x1b[H"),
            "Should contain reset commands"
        );
        assert!(output.contains("Test line"), "Should contain history");

        // Ensure we flushed the writer
        assert!(
            terminal.writer.flush_count > 0,
            "Should have flushed writer"
        );
    }
}
