//! Layer 2a: Screen state tracking via `alacritty_terminal` (echo-agent-pty).
//!
//! Parses raw PTY output through a headless terminal emulator and provides
//! queries for what the user would see on screen.

use echo_agent_pty::styled::StyledLine;
use echo_agent_pty::term::{ScreenOpts, ScreenOutput, SessionListener, Terminal};

/// Tracks the virtual terminal screen state by feeding raw PTY output
/// through an `alacritty_terminal`-based headless terminal (via echo-agent-pty).
pub struct ScreenTracker {
    terminal: Terminal,
    /// Receives terminal-generated replies (cursor-position reports, device
    /// attributes, color queries, …) the emulator emits while parsing input.
    /// Drained by [`ScreenTracker::drain_responses`] so the harness can forward
    /// them back to the child (real terminals answer these automatically).
    pty_write_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
}

impl ScreenTracker {
    /// Create a new tracker for a terminal with the given dimensions.
    pub fn new(rows: u16, cols: u16) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let listener = SessionListener::new(tx);
        Self {
            terminal: Terminal::new(cols, rows, listener),
            pty_write_rx: rx,
        }
    }

    /// Feed raw PTY output bytes into the terminal emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.feed(bytes);
    }

    /// Drain any terminal-generated replies queued while parsing fed input
    /// (cursor-position reports answering `ESC[6n`, device attributes, color
    /// queries, …), concatenated in order. Empty when nothing was queued.
    ///
    /// These MUST be written back to the PTY or programs that probe the
    /// terminal will hang or time out — most relevant here, the inline
    /// viewport's startup cursor-position query that minimal mode depends on
    /// (a timeout there downgrades `--minimal` to full-screen inline). A real
    /// terminal answers automatically; the harness forwards these in
    /// [`crate::PtyHarness::update`] when response forwarding is enabled.
    pub fn drain_responses(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(bytes) = self.pty_write_rx.try_recv() {
            out.extend_from_slice(&bytes);
        }
        out
    }

    /// Return structured screen contents (no escape codes).
    pub fn output(&self) -> ScreenOutput {
        self.terminal.screen_content(&ScreenOpts::default())
    }

    /// Return the full text contents of the screen (no escape codes).
    pub fn contents(&self) -> String {
        self.output().lines.join("\n")
    }

    /// Check whether the screen contains the given text substring.
    pub fn contains(&self, text: &str) -> bool {
        self.contents().contains(text)
    }

    /// Return the current cursor position as `(row, col)` (0-indexed, matching
    /// the original vt100 convention used by existing tests).
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.terminal.cursor_position();
        // echo-agent-pty cursor is 1-indexed; the harness API is 0-indexed.
        (
            (pos.row as u16).saturating_sub(1),
            (pos.col as u16).saturating_sub(1),
        )
    }

    /// Resize the virtual terminal to new dimensions.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.terminal.resize(cols, rows);
    }

    /// Return the full screen with style information for visual artifacts.
    pub fn styled(&self) -> Vec<StyledLine> {
        self.terminal.screen_styled(&ScreenOpts::default())
    }

    /// Render the current screen as an HTML document.
    pub fn html(&self) -> String {
        self.terminal.screen_html(&ScreenOpts::default())
    }

    /// Access the underlying echo-agent-pty `Terminal` for advanced queries
    /// (styled output, scrollback, terminal modes, etc.).
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Number of lines in the terminal's scrollback history — content that has
    /// scrolled *above* the visible screen. This is where minimal mode's
    /// committed conversation blocks land (printed via `insert_before`).
    pub fn scrollback_count(&self) -> usize {
        self.terminal.scrollback_count()
    }

    /// The full scrollback history as text, oldest line first.
    pub fn scrollback_text(&self) -> String {
        let n = self.terminal.scrollback_count();
        self.terminal
            .scrollback_lines(n)
            .into_iter()
            .map(|l| l.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Scrollback history plus the visible screen, joined oldest→newest:
    /// everything a user could see by scrolling up. Minimal-mode committed
    /// content may be in either region depending on how much has accumulated,
    /// so assertions on committed output should use this.
    pub fn full_text(&self) -> String {
        let sb = self.scrollback_text();
        let screen = self.contents();
        if sb.is_empty() {
            screen
        } else {
            format!("{sb}\n{screen}")
        }
    }

    /// Whether scrollback + visible screen contains `text`.
    pub fn full_contains(&self, text: &str) -> bool {
        self.full_text().contains(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines pushed above a small screen must be readable via the scrollback
    /// helpers — the property minimal-mode e2e tests rely on to assert that a
    /// committed block reached native scrollback.
    #[test]
    fn scrolled_off_lines_are_captured_by_scrollback_helpers() {
        // 3-row screen; print 8 numbered lines so the first ones scroll off.
        let mut s = ScreenTracker::new(3, 20);
        for i in 1..=8 {
            s.feed(format!("line{i}\r\n").as_bytes());
        }
        // The earliest lines are no longer on the visible screen…
        assert!(
            !s.contains("line1"),
            "line1 should have scrolled off-screen"
        );
        // …but they are in scrollback, and full_text sees everything.
        assert!(s.scrollback_count() >= 5, "expected scrolled-off history");
        assert!(s.scrollback_text().contains("line1"));
        assert!(s.full_contains("line1"));
        assert!(s.full_contains("line8"));
    }

    /// A DSR cursor-position query (`ESC[6n`) must produce a forwardable reply
    /// (a CPR `ESC[<row>;<col>R`) — the mechanism minimal-mode tests rely on so
    /// the inline viewport's startup cursor query completes. Without forwarding,
    /// `--minimal` silently downgrades to full-screen inline.
    #[test]
    fn drain_responses_answers_cursor_position_query() {
        let mut s = ScreenTracker::new(24, 80);
        // Nothing queued before any query is fed.
        assert!(s.drain_responses().is_empty());

        s.feed(b"\x1b[6n");
        let reply = s.drain_responses();
        assert!(
            reply.starts_with(b"\x1b[") && reply.ends_with(b"R"),
            "expected a cursor-position report, got {:?}",
            String::from_utf8_lossy(&reply)
        );

        // Drained exactly once — no duplicate delivery on the next call.
        assert!(s.drain_responses().is_empty());
    }
}
