//! Interactive markdown table rendering playground.
//!
//! Run with:
//!   cargo run -p echo-agent-markdown --features playground --bin md-table-test
//!
//! Controls:
//!   Space        — toggle textarea focus
//!   Alt/Ctrl/Shift+Enter — submit markdown & defocus (when textarea focused)
//!   h / Left     — shrink render width  (when unfocused)
//!   l / Right    — grow render width    (when unfocused)
//!   Esc          — quit (always)

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidgetRef, Wrap};

use echo_agent_markdown::{
    MarkdownBuffers, MarkdownStyle, StreamingMarkdownRenderer,
    render_markdown_ratatui_with_buffers_width,
};
use echo_agent_ratatui_textarea::{TextArea, TextAreaState};

// ── Tokyo Night Storm palette (matches echo-agent-pager) ──────────────────────

#[path = "playground_common.rs"]
mod playground_common;
use playground_common::{get_syntect, md_style};

const MD_STYLE: MarkdownStyle = md_style(anstyle::Style::new());

// ── Compute minimum render width ─────────────────────────────────────────────

/// Minimum width = 4k + 1, where k = number of table columns.
/// Columns = max(`|` count per line) - 1  (the outer pipes are borders).
/// This ensures every column gets at least 1 char + padding.  Floor of 10.
fn min_render_width(source: &str) -> usize {
    let max_pipes = source
        .lines()
        .map(|line| line.chars().filter(|&c| c == '|').count())
        .max()
        .unwrap_or(0);
    let num_cols = max_pipes.saturating_sub(1);
    if num_cols == 0 {
        10
    } else {
        (4 * num_cols + 1).max(10)
    }
}

// ── Render helpers ───────────────────────────────────────────────────────────

/// One-shot full render at a given width.
fn render_full(source: &str, width: usize) -> Vec<Line<'static>> {
    let mut buffers = MarkdownBuffers::new();
    let (output, _) = render_markdown_ratatui_with_buffers_width(
        source,
        MD_STYLE,
        true,
        &mut buffers,
        Some(get_syntect()),
        Some(width),
    );
    output.lines
}

/// Streaming render: feed each char individually, return final lines.
fn render_streaming(source: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = StreamingMarkdownRenderer::new(MD_STYLE, true);
    renderer.set_max_table_width(Some(width));
    for ch in source.chars() {
        renderer.push_and_render(&ch.to_string(), Some(get_syntect()));
    }
    renderer.view().lines.to_vec()
}

// ── App state ────────────────────────────────────────────────────────────────

const DEFAULT_MARKDOWN: &str = "\
| A | B | C |
|---|---|---|
| 1 | 2 | 3 |
";

struct App {
    textarea: TextArea,
    textarea_state: TextAreaState,
    textarea_focused: bool,
    textarea_area: Rect,
    render_width: usize,
    source: String,
    full_lines: Vec<Line<'static>>,
    streaming_lines: Vec<Line<'static>>,
}

impl App {
    fn new() -> Self {
        let mut textarea = TextArea::new();
        textarea.set_text(DEFAULT_MARKDOWN);
        textarea.show_scrollbar = false;
        let source = DEFAULT_MARKDOWN.to_string();
        let width = 24usize;
        let full_lines = render_full(&source, width);
        let streaming_lines = render_streaming(&source, width);
        Self {
            textarea,
            textarea_state: TextAreaState::default(),
            textarea_focused: true,
            textarea_area: Rect::default(),
            render_width: width,
            source,
            full_lines,
            streaming_lines,
        }
    }

    fn rerender(&mut self) {
        self.source = self.textarea.text().to_string();
        let min_w = min_render_width(&self.source);
        if self.render_width < min_w {
            self.render_width = min_w;
        }
        self.full_lines = render_full(&self.source, self.render_width);
        self.streaming_lines = render_streaming(&self.source, self.render_width);
    }

    fn adjust_width(&mut self, delta: isize) {
        let min_w = min_render_width(&self.source);
        let new_w = (self.render_width as isize + delta).max(min_w as isize) as usize;
        if new_w != self.render_width {
            self.render_width = new_w;
            self.full_lines = render_full(&self.source, self.render_width);
            self.streaming_lines = render_streaming(&self.source, self.render_width);
        }
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    // Terminal setup
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            let ev = event::read()?;

            match &ev {
                // ── Global: Ctrl-Q quits from anywhere ──
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => break,

                // ── Focused: defocus on Esc or Tab ──
                Event::Key(KeyEvent {
                    code: KeyCode::Esc | KeyCode::Tab,
                    ..
                }) if app.textarea_focused => {
                    app.textarea_focused = false;
                }

                // ── Unfocused: quit on q, Ctrl-C, Ctrl-D ──
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                }) if !app.textarea_focused => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c') | KeyCode::Char('d'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) if !app.textarea_focused => break,

                // ── Unfocused: Space or Enter to re-focus ──
                Event::Key(KeyEvent {
                    code: KeyCode::Char(' ') | KeyCode::Enter,
                    ..
                }) if !app.textarea_focused => {
                    app.textarea_focused = true;
                }

                // ── Unfocused: width controls ──
                Event::Key(KeyEvent {
                    code: KeyCode::Char('h') | KeyCode::Left,
                    ..
                }) if !app.textarea_focused => {
                    app.adjust_width(-1);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l') | KeyCode::Right,
                    ..
                }) if !app.textarea_focused => {
                    app.adjust_width(1);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('H'),
                    ..
                }) if !app.textarea_focused => {
                    app.adjust_width(-5);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('L'),
                    ..
                }) if !app.textarea_focused => {
                    app.adjust_width(5);
                }

                // ── Focused: forward keys to textarea, live re-render ──
                Event::Key(key) if app.textarea_focused => {
                    app.textarea.input(*key);
                    app.rerender();
                }

                // ── Focused: forward mouse to textarea ──
                Event::Mouse(mouse) if app.textarea_focused => {
                    app.textarea
                        .handle_mouse(*mouse, app.textarea_area, app.textarea_state);
                }

                _ => {}
            }
        }
    }

    // Terminal cleanup
    stdout().execute(DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

// ── Drawing ──────────────────────────────────────────────────────────────────

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();

    // Layout: header (1) | textarea (flexible) | full render | streaming render
    //
    // We compute the heights we need for the render panels, then give the rest
    // to the textarea.

    let render_w = app.render_width as u16;
    let full_height = wrapped_line_count(&app.full_lines, render_w).max(1) + 2; // +2 for border
    let stream_height = wrapped_line_count(&app.streaming_lines, render_w).max(1) + 2;

    // Detect mismatches between full and streaming
    let mismatch = detect_mismatch(&app.full_lines, &app.streaming_lines);

    let header_height = 2u16;
    let render_height = full_height + stream_height;
    let textarea_min = 5u16;
    let textarea_height = size
        .height
        .saturating_sub(header_height + render_height)
        .max(textarea_min);

    let chunks = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Length(textarea_height),
        Constraint::Length(full_height),
        Constraint::Length(stream_height),
    ])
    .split(size);

    // ── Header ──
    let focus_indicator = if app.textarea_focused {
        Span::styled(
            " EDITING ",
            Style::default().fg(Color::Black).bg(Color::Green),
        )
    } else {
        Span::styled(
            " VIEW ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    };

    let width_info = Span::styled(
        format!(
            " width: {} (min: {}) ",
            app.render_width,
            min_render_width(&app.source)
        ),
        Style::default().fg(Color::Cyan),
    );

    let keys = if app.textarea_focused {
        Span::styled(
            " (live) Esc/Tab: defocus ",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::styled(
            " Space/Enter: edit | h/l: width -/+ | H/L: -5/+5 | q/^C/^D: quit ",
            Style::default().fg(Color::DarkGray),
        )
    };

    let mismatch_indicator = if mismatch {
        Span::styled(
            " MISMATCH! ",
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" OK ", Style::default().fg(Color::Black).bg(Color::Green))
    };

    let header = Paragraph::new(vec![
        Line::from(vec![
            focus_indicator,
            Span::raw(" "),
            width_info,
            Span::raw(" "),
            mismatch_indicator,
        ]),
        Line::from(keys),
    ]);
    f.render_widget(header, chunks[0]);

    // ── Textarea ──
    let border_color = if app.textarea_focused {
        Color::Green
    } else {
        Color::DarkGray
    };
    let textarea_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            " Markdown Input ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    let textarea_inner = textarea_block.inner(chunks[1]);
    f.render_widget(textarea_block, chunks[1]);
    app.textarea_area = textarea_inner;
    (&app.textarea).render_ref(textarea_inner, f.buffer_mut(), &mut app.textarea_state);

    if app.textarea_focused
        && let Some((cx, cy)) = app
            .textarea
            .cursor_pos_with_state(textarea_inner, app.textarea_state)
    {
        f.set_cursor_position((cx, cy));
    }

    // ── Full render panel ──
    let full_title = format!(" full: {} ", app.render_width);
    render_panel(f, chunks[2], &full_title, &app.full_lines, render_w, false);

    // ── Streaming render panel ──
    let stream_title = format!(" stream: {} ", app.render_width);
    render_panel(
        f,
        chunks[3],
        &stream_title,
        &app.streaming_lines,
        render_w,
        mismatch,
    );
}

/// Count the number of visual rows a set of lines occupies when soft-wrapped
/// to `width` columns.  Each line takes ceil(display_width / width) rows,
/// with a minimum of 1 row per line (empty lines still occupy a row).
fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return lines.len() as u16;
    }
    let w = width as usize;
    lines
        .iter()
        .map(|line| {
            let display_w: usize = line
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if display_w == 0 {
                1u16
            } else {
                display_w.div_ceil(w) as u16 // ceil division
            }
        })
        .sum()
}

/// Render a bordered panel whose *inner* width is exactly `inner_w`.
///
/// Content is soft-wrapped with `Wrap { trim: false }` so long non-table
/// lines fold inside the box.  The box is left-aligned within the available
/// `area`.  If `is_error` is true the border turns red.
fn render_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    lines: &[Line<'static>],
    inner_w: u16,
    is_error: bool,
) {
    // The block border adds 1 column on each side, so outer width = inner_w + 2.
    let outer_w = (inner_w + 2).min(area.width);
    let box_area = Rect {
        x: area.x,
        y: area.y,
        width: outer_w,
        height: area.height,
    };

    let border_color = if is_error {
        Color::Red
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    let para = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
    f.render_widget(para, inner);
}

/// Detect if full and streaming outputs differ in text content.
fn detect_mismatch(full: &[Line<'static>], streaming: &[Line<'static>]) -> bool {
    if full.len() != streaming.len() {
        return true;
    }
    for (f_line, s_line) in full.iter().zip(streaming.iter()) {
        let f_text: String = f_line.spans.iter().map(|s| s.content.as_ref()).collect();
        let s_text: String = s_line.spans.iter().map(|s| s.content.as_ref()).collect();
        if f_text != s_text {
            return true;
        }
    }
    false
}
