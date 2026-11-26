use std::io::{self, Write};

use crossterm::{
    QueueableCommand as _,
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};
use ratatui::{
    layout::{Rect, Size},
    prelude::Backend,
};

use crate::Terminal;

/// Trait for terminal operations needed by emit_to_scrollback and other functions.
pub trait TerminalLike {
    /// The writer type that will be used for output
    type Writer: Write;

    /// Get the terminal size
    fn size(&self) -> io::Result<Size>;

    /// Get the current viewport area
    fn viewport_area(&self) -> Rect;

    /// Clear the terminal
    fn clear(&mut self) -> io::Result<()>;

    /// Reset the back buffer without clearing the screen
    fn reset_back_buffer(&mut self);

    /// Set the viewport area
    fn set_viewport_area(&mut self, area: Rect);

    /// Get a mutable reference to the writer
    fn writer_mut(&mut self) -> &mut Self::Writer;
}

// Implementation for our Terminal with any Backend that implements Write
impl<B: Backend + Write> TerminalLike for Terminal<B> {
    type Writer = B;

    fn size(&self) -> io::Result<Size> {
        self.backend().size()
    }

    fn viewport_area(&self) -> Rect {
        self.viewport_area()
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear()
    }

    fn reset_back_buffer(&mut self) {
        self.reset_back_buffer()
    }

    fn set_viewport_area(&mut self, area: Rect) {
        self.set_viewport_area(area)
    }

    fn writer_mut(&mut self) -> &mut Self::Writer {
        self.backend_mut()
    }
}

/// Execute a function with synchronized terminal output to prevent flicker
///
/// This wraps the provided function with terminal synchronized output mode,
/// making all terminal operations within the function atomic.
/// Supported by most modern terminals (iTerm2, kitty, WezTerm, Windows Terminal, etc.)
/// Gracefully ignored by terminals that don't support it.
///
/// IMPORTANT: if the closure panics, it is responsibility of the caller to clean
/// this up, otherwise the terminal may hang forever (depends on the terminal / mux).
pub fn with_synchronized_output<T, F, R>(terminal: &mut T, f: F) -> io::Result<R>
where
    T: TerminalLike,
    F: FnOnce(&mut T) -> io::Result<R>,
{
    // Begin synchronized output
    terminal.writer_mut().queue(BeginSynchronizedUpdate)?;

    // Execute the provided function
    let result = f(terminal);

    // End synchronized output and flush
    terminal.writer_mut().queue(EndSynchronizedUpdate)?;
    terminal.writer_mut().flush()?;

    result
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::tests::MockTerminal;

    use super::*;

    #[test]
    fn test_synchronized_output() {
        let mut terminal = MockTerminal::new(80, 25, 3);

        // Use synchronized output wrapper
        let result = with_synchronized_output(&mut terminal, |terminal| {
            _ = terminal.writer_mut().write(b"Test content")?;
            terminal.writer_mut().flush()?;
            Ok(())
        });

        assert!(result.is_ok());

        // Check that synchronized output markers were written
        let buffer = &terminal.writer.buffer;
        let text = String::from_utf8_lossy(buffer);

        // Should contain begin and end synchronized update sequences
        assert!(
            text.contains("\x1b[?2026h"),
            "Should have begin synchronized update"
        );
        assert!(
            text.contains("\x1b[?2026l"),
            "Should have end synchronized update"
        );

        // Content should be between the markers
        assert!(text.contains("Test content"));

        // Should have flushed (once in emit_to_scrollback, once in with_synchronized_output)
        assert_eq!(terminal.writer.flush_count, 2);
    }
}
