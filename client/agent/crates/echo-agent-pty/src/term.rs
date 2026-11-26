//! Wrapper around `alacritty_terminal::Term` for headless terminal emulation.
//!
//! Provides a simplified interface for feeding PTY output into the terminal
//! state machine and reading back screen content as text, styled JSON, or HTML.

use std::ops::Range;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi;

use crate::styled::{self, StyledLine};

/// Event listener that captures `PtyWrite` events for forwarding back to PTY.
///
/// When the terminal emulator needs to respond to device status queries (DSR),
/// color queries, etc., it emits `Event::PtyWrite`. These MUST be forwarded
/// back to the PTY, otherwise programs like vim/tmux will hang waiting for
/// a response.
#[derive(Clone)]
pub struct SessionListener {
    pty_write_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl SessionListener {
    pub fn new(pty_write_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self { pty_write_tx }
    }
}

impl EventListener for SessionListener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            let _ = self.pty_write_tx.send(text.into_bytes());
        }
    }
}

/// Options for querying screen content.
#[derive(Debug, Clone, Default)]
pub struct ScreenOpts {
    /// Row range (1-indexed, inclusive). None = all rows.
    pub rows: Option<Range<usize>>,
    /// Column range (1-indexed, inclusive). None = all columns.
    pub cols: Option<Range<usize>>,
    /// If set, replace the character at cursor position with this char.
    pub cursor_char: Option<char>,
    /// Include trailing empty lines (default: trim them).
    pub include_empty: bool,
}

/// Plain text screen output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScreenOutput {
    pub lines: Vec<String>,
    pub cursor: CursorPosition,
    pub size: TerminalSize,
}

/// Cursor position (1-indexed).
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CursorPosition {
    pub row: usize,
    pub col: usize,
}

/// Terminal dimensions.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TerminalSize {
    pub cols: usize,
    pub rows: usize,
}

/// Active terminal modes — tells callers what the running program has enabled.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TerminalModes {
    /// Alternate screen buffer is active (vim, less, htop, etc.).
    pub alt_screen: bool,
    /// Bracketed paste mode — input pasted between ESC[200~ / ESC[201~.
    pub bracketed_paste: bool,
    /// Application cursor keys (arrow keys send SS3 instead of CSI).
    pub app_cursor: bool,
    /// Application keypad mode.
    pub app_keypad: bool,
    /// Line-wrap mode (auto-wrap at right margin).
    pub line_wrap: bool,
    /// Origin mode (cursor addressing relative to scroll region).
    pub origin: bool,
    /// Cursor is visible.
    pub show_cursor: bool,
    /// Insert mode.
    pub insert: bool,
    /// LF/NL mode (linefeed also does carriage return).
    pub linefeed_newline: bool,
    /// Focus in/out reporting enabled.
    pub focus_in_out: bool,
    /// Mouse click reporting enabled.
    pub mouse_reporting: bool,
}

/// A single line from scrollback history.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScrollbackLine {
    /// 1-indexed offset from the bottom of scrollback (1 = most recent).
    pub offset: usize,
    /// Text content of the line.
    pub text: String,
}

/// A simple `Dimensions` impl for creating a `Term`.
struct TermDimensions {
    columns: usize,
    screen_lines: usize,
}

impl TermDimensions {
    fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns,
            screen_lines,
        }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Headless terminal emulator wrapping `alacritty_terminal`.
pub struct Terminal {
    term: Term<SessionListener>,
    parser: ansi::Processor,
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(cols: u16, rows: u16, listener: SessionListener) -> Self {
        let size = TermDimensions::new(cols as usize, rows as usize);
        let config = Config::default();
        let term = Term::new(config, &size, listener);
        let parser = ansi::Processor::new();

        Self { term, parser }
    }

    /// Feed raw bytes from PTY output into the terminal emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Get the cursor position (1-indexed).
    pub fn cursor_position(&self) -> CursorPosition {
        let point = self.term.grid().cursor.point;
        CursorPosition {
            row: point.line.0 as usize + 1,
            col: point.column.0 + 1,
        }
    }

    /// Get terminal dimensions.
    pub fn size(&self) -> TerminalSize {
        TerminalSize {
            cols: self.term.columns(),
            rows: self.term.screen_lines(),
        }
    }

    /// Resize the terminal.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = TermDimensions::new(cols as usize, rows as usize);
        self.term.resize(size);
    }

    /// Get the active terminal modes.
    pub fn terminal_modes(&self) -> TerminalModes {
        let mode = self.term.mode();
        TerminalModes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            line_wrap: mode.contains(TermMode::LINE_WRAP),
            origin: mode.contains(TermMode::ORIGIN),
            show_cursor: mode.contains(TermMode::SHOW_CURSOR),
            insert: mode.contains(TermMode::INSERT),
            linefeed_newline: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            focus_in_out: mode.contains(TermMode::FOCUS_IN_OUT),
            mouse_reporting: mode.intersects(
                TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
            ),
        }
    }

    /// Number of lines in the scrollback buffer.
    pub fn scrollback_count(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Read scrollback lines. `count` limits how many to return (from the
    /// bottom / most-recent). Returns them in chronological order (oldest first).
    pub fn scrollback_lines(&self, count: usize) -> Vec<ScrollbackLine> {
        let grid = self.term.grid();
        let history = grid.history_size();
        let n = count.min(history);
        let num_cols = grid.columns();

        let mut lines = Vec::with_capacity(n);
        // history lines are at negative indices: Line(-1) is most recent
        for offset in (1..=n).rev() {
            let row = &grid[Line(-(offset as i32))];
            let mut text = String::new();
            for col_idx in 0..num_cols {
                let cell = &row[Column(col_idx)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    for &c in zw {
                        text.push(c);
                    }
                }
            }
            lines.push(ScrollbackLine {
                offset,
                text: text.trim_end().to_string(),
            });
        }
        lines
    }

    /// Read screen content as plain text lines.
    pub fn screen_content(&self, opts: &ScreenOpts) -> ScreenOutput {
        let grid = self.term.grid();
        let num_lines = grid.screen_lines();
        let num_cols = grid.columns();
        let cursor = self.cursor_position();

        let (row_start, row_end) = resolve_range(&opts.rows, num_lines);
        let (col_start, col_end) = resolve_range(&opts.cols, num_cols);

        let mut lines = Vec::new();

        for line_idx in row_start..row_end {
            let row = &grid[Line(line_idx as i32)];
            let mut text = String::new();

            for col_idx in col_start..col_end {
                let cell = &row[Column(col_idx)];

                // Skip wide char spacers.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }

                // Replace cursor position if requested.
                let is_cursor = cursor.row == line_idx + 1 && cursor.col == col_idx + 1;
                if is_cursor && let Some(cursor_char) = opts.cursor_char {
                    text.push(cursor_char);
                } else {
                    text.push(cell.c);
                    if let Some(zw) = cell.zerowidth() {
                        for &c in zw {
                            text.push(c);
                        }
                    }
                }
            }

            lines.push(text);
        }

        // Trim trailing empty lines unless include_empty is set.
        if !opts.include_empty {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
        }

        // Right-trim each line.
        for line in &mut lines {
            let trimmed = line.trim_end().to_string();
            *line = trimmed;
        }

        ScreenOutput {
            lines,
            cursor,
            size: self.size(),
        }
    }

    /// Read screen content with full style information.
    pub fn screen_styled(&self, opts: &ScreenOpts) -> Vec<StyledLine> {
        let grid = self.term.grid();
        let num_lines = grid.screen_lines();
        let num_cols = grid.columns();
        let cursor = self.cursor_position();

        let (row_start, row_end) = resolve_range(&opts.rows, num_lines);
        let (col_start, col_end) = resolve_range(&opts.cols, num_cols);

        let mut result = Vec::new();

        for line_idx in row_start..row_end {
            let row = &grid[Line(line_idx as i32)];
            let styled_line =
                styled::extract_styled_line(row, col_start, col_end, line_idx + 1, &cursor, opts);
            result.push(styled_line);
        }

        // Trim trailing empty styled lines unless include_empty is set.
        if !opts.include_empty {
            while result.last().is_some_and(|l| l.runs.is_empty()) {
                result.pop();
            }
        }

        result
    }

    /// Render screen content as HTML.
    pub fn screen_html(&self, opts: &ScreenOpts) -> String {
        let styled = self.screen_styled(opts);
        styled::render_html(&styled, &self.cursor_position(), &self.size())
    }
}

/// Resolve an optional 1-indexed range to 0-indexed (start, end).
fn resolve_range(range: &Option<Range<usize>>, max: usize) -> (usize, usize) {
    match range {
        Some(r) => {
            let start = r.start.saturating_sub(1).min(max);
            let end = r.end.min(max);
            (start, end)
        }
        None => (0, max),
    }
}
