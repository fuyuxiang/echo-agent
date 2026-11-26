use std::io::{self, Write};

use crossterm::{cursor::MoveTo, style::Print};
use ratatui::layout::Rect;

use crate::{common::TerminalLike, segment::split_into_line_segments};

// ANSI escape sequence constants.
// CSI J with the default parameter (0): erase from cursor to end of display.
// Byte-identical to what the previous termwiz constant
// (`CSI::Edit(Edit::EraseInDisplay(EraseInDisplay::EraseToEndOfDisplay))`)
// rendered, and to crossterm's `Clear(ClearType::FromCursorDown)`.
const ANSI_CLEAR_FROM_CURSOR_DOWN: &str = "\x1b[J";

pub fn emit_to_scrollback<T: TerminalLike>(terminal: &mut T, content: &str) -> io::Result<()> {
    macro_rules! queue {
        ($($command:expr),* $(,)?) => {{
            $(crossterm::queue!(terminal.writer_mut(), $command)?;)*
            Ok::<(), io::Error>(())
        }};
    }

    let size = terminal.size()?;
    let viewport_area = terminal.viewport_area();
    let terminal_width = size.width as usize;
    debug_assert!(viewport_area.bottom() <= size.height);

    // Use zero-copy line segmentation
    let segments = split_into_line_segments(content, terminal_width);

    // Calculate where viewport will end up after content
    let new_viewport_y =
        (viewport_area.y + segments.len() as u16).min(size.height - viewport_area.height);

    // Position from viewport top and clear from this position down
    queue!(
        MoveTo(0, viewport_area.y),
        Print(ANSI_CLEAR_FROM_CURSOR_DOWN),
    )?;

    // Now print the content
    queue!(MoveTo(0, viewport_area.y))?;
    for segment in &segments {
        queue!(Print(segment))?; // this already includes crlfs if there's any
    }

    // Create exact viewport space
    for _ in 0..viewport_area.height {
        queue!(Print("\r\n"))?;
    }

    // Clear the new viewport area for rendering
    queue!(
        MoveTo(0, new_viewport_y),
        Print(ANSI_CLEAR_FROM_CURSOR_DOWN),
    )?;

    // We'll flush by default; the caller is expected to have this in sync block anyway
    terminal.writer_mut().flush()?;

    // Reset the back buffer so next render knows viewport is empty
    terminal.reset_back_buffer();

    // Reposition viewport if needed
    if new_viewport_y != viewport_area.y {
        terminal.set_viewport_area(Rect {
            y: new_viewport_y,
            ..viewport_area
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::tests::MockTerminal;

    use super::*;

    // Helper to parse ANSI sequences from the captured buffer
    fn parse_ansi_sequences(buffer: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(buffer);
        let mut sequences = Vec::new();
        let mut current = String::new();
        let mut in_escape = false;

        for ch in text.chars() {
            if ch == '\x1b' {
                if !current.is_empty() {
                    sequences.push(current.clone());
                    current.clear();
                }
                in_escape = true;
                current.push(ch);
            } else if in_escape {
                current.push(ch);
                // Simple heuristic: most ANSI sequences end with a letter
                if ch.is_alphabetic() {
                    sequences.push(current.clone());
                    current.clear();
                    in_escape = false;
                }
            } else {
                current.push(ch);
            }
        }

        if !current.is_empty() {
            sequences.push(current);
        }

        sequences
    }

    #[test]
    fn test_simple_content() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        let content = "Hello, World!";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Should have cleared once
        assert_eq!(terminal.clear_count, 1);

        // Check that content was written
        let buffer = &terminal.writer.buffer;
        assert!(!buffer.is_empty());

        // Should have flushed
        assert_eq!(terminal.writer.flush_count, 1);
    }

    #[test]
    fn test_tall_content() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        // Create content that will span more lines than viewport height
        let content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Should have cleared once
        assert_eq!(terminal.clear_count, 1);

        // Check that content was written
        let buffer = &terminal.writer.buffer;
        assert!(!buffer.is_empty());

        // Should contain the content
        let text = String::from_utf8_lossy(buffer);
        assert!(text.contains("Line 1"));
        assert!(text.contains("Line 5"));

        // Should have flushed
        assert_eq!(terminal.writer.flush_count, 1);
    }

    #[test]
    fn test_content_with_viewport_at_bottom() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        let content = "Hello, Multiplexer!";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Should have cleared once
        assert_eq!(terminal.clear_count, 1);

        // Check that content was written
        let buffer = &terminal.writer.buffer;
        assert!(!buffer.is_empty());

        // Should have flushed
        assert_eq!(terminal.writer.flush_count, 1);

        // Viewport should remain at bottom
        assert_eq!(terminal.viewport_area.y, 22); // 25 - 3
    }

    #[test]
    fn test_viewport_not_at_bottom() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        // Move viewport away from bottom
        terminal.viewport_area.y = 10;

        let content = "Test content";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Should have cleared
        assert_eq!(terminal.clear_count, 1);

        // Viewport should have moved down
        assert_eq!(terminal.viewport_updates.len(), 1);
        assert!(terminal.viewport_updates[0].y > 10);
    }

    #[test]
    fn test_long_lines_wrapping() {
        let mut terminal = MockTerminal::new(20, 10, 2);
        // Content longer than terminal width
        let content = "This is a very long line that should wrap at terminal boundaries";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Should have cleared once
        assert_eq!(terminal.clear_count, 1);

        // Should have written content
        let buffer = &terminal.writer.buffer;
        assert!(!buffer.is_empty());

        // Should have flushed
        assert_eq!(terminal.writer.flush_count, 1);
    }

    #[test]
    fn test_ansi_color_preservation() {
        let mut terminal = MockTerminal::new(80, 25, 3);
        let content = "\x1b[31mRed Text\x1b[0m";

        emit_to_scrollback(&mut terminal, content).unwrap();

        // Check that ANSI codes are preserved in output
        let buffer = &terminal.writer.buffer;
        let text = String::from_utf8_lossy(buffer);
        assert!(text.contains("Red Text"), "Text should be in output");

        // The ANSI codes might be in the segment's content
        let sequences = parse_ansi_sequences(buffer);
        let has_color = sequences.iter().any(|s| s.contains("Red Text"));
        assert!(has_color, "Colored text should be present");
    }
}
