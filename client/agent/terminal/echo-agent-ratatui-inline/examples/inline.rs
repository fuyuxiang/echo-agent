use std::{
    io,
    time::{Duration, Instant},
};

use ansi_width::ansi_width;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    style::Color as CColor,
};
use lipsum::lipsum;
use ratatui::{
    TerminalOptions, Viewport,
    prelude::CrosstermBackend,
    style::{Color, Style},
    widgets::Block,
};

use echo_agent_ratatui_inline::{
    Terminal, emit_to_scrollback, resize_purge_rerender, resize_viewport_height,
    with_synchronized_output,
};

/// Build a line with markers for visualization
fn build_marked_line(line_num: usize, content: &str) -> String {
    format!("[[{:02}]] {} [[{:02}]]", line_num, content, line_num)
}

/// Generate test content based on pattern
fn generate_test_line(line_num: usize, terminal_width: usize) -> String {
    let i = line_num - 1;
    let mut line = match i % 3 {
        0 => lipsum(4),  // Short line
        1 => lipsum(40), // Long line
        2 => {
            // Unicode string with ANSI colors and hyperlink for testing
            let unicode_with_ansi = "\x1b[31m😀\u{200D}\x1b[0m\x1b[32mé\x1b[0m中\u{0300}\x1b[34mX\x1b[0m\x1b]8;;https://example.com\x1b\\H\x1b]8;;\x1b\\";
            let visual_width = ansi_width(unicode_with_ansi);
            let marker_adjustment = if i % 6 >= 3 { 7 } else { 14 };
            let remaining_width = terminal_width * 2 - marker_adjustment - visual_width;
            format!("{unicode_with_ansi}{}", "B".repeat(remaining_width))
        }
        _ => unreachable!(),
    };

    // Add newline prefix for cases 3-5, alternating between \n and \r\n
    if i % 6 >= 3 {
        line = format!("{}\n{}\r\n{line}", lipsum(2), lipsum(8));
    }

    line
}

/// Generate tall content that's 1.5x terminal height
fn generate_tall_content(terminal_width: usize, terminal_height: usize) -> String {
    let num_lines = terminal_height;
    let mut lines = Vec::new();
    for i in 1..=num_lines {
        lines.push(format!(
            "Line {i:03}/{num_lines:03}: {}",
            if i % 2 == 1 {
                lipsum((terminal_width / 25).max(1))
            } else {
                lipsum((terminal_width / 5).max(1))
            }
        ));
    }
    lines.join("\n")
}

/// Build content with ANSI color codes
fn colorize_content(content: &str, color: CColor) -> String {
    let color_code = match color {
        CColor::Black => 30,
        CColor::Red | CColor::DarkRed => 31,
        CColor::Green | CColor::DarkGreen => 32,
        CColor::Yellow | CColor::DarkYellow => 33,
        CColor::Blue | CColor::DarkBlue => 34,
        CColor::Magenta | CColor::DarkMagenta => 35,
        CColor::Cyan | CColor::DarkCyan => 36,
        CColor::White | CColor::Grey | CColor::DarkGrey => 37,
        _ => 37, // Default to white for RGB or other colors
    };
    format!("\x1b[{}m{}\x1b[0m", color_code, content)
}

fn init_terminal(inline_height: u16) -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        fn try_restore() -> io::Result<()> {
            crossterm::terminal::disable_raw_mode()?;
            Ok(())
        }

        if let Err(err) = try_restore() {
            eprintln!("Failed to restore terminal: {err}");
        }
        hook(info);
    }));
    let backend = CrosstermBackend::new(io::stdout());
    let options = TerminalOptions {
        viewport: Viewport::Inline(inline_height),
    };
    let terminal = Terminal::with_options(backend, options)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    // Clear only the viewport, preserving log content above it
    terminal.clear()?;
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}

enum KeyAction {
    Quit,
    Normal,
    Tall,
    IncreaseViewport,
    DecreaseViewport,
}

/// Spinner animation state
struct Spinner {
    frames: Vec<&'static str>,
    current: usize,
    interval: Duration,
    next_frame_time: Instant,
}

impl Spinner {
    fn new() -> Self {
        let frames = vec!["⠋", "⠙", "⠚", "⠞", "⠖", "⠦", "⠴", "⠲", "⠳", "⠓"];
        Self {
            frames,
            current: 0,
            interval: Duration::from_millis(100),
            next_frame_time: Instant::now() + Duration::from_millis(100),
        }
    }

    fn tick(&mut self) {
        self.current = (self.current + 1) % self.frames.len();
        self.next_frame_time = Instant::now() + self.interval;
    }

    fn current_frame(&self) -> &str {
        self.frames[self.current]
    }

    fn time_until_next_frame(&self) -> Duration {
        self.next_frame_time
            .saturating_duration_since(Instant::now())
    }
}

enum PollResult {
    KeyPressed(KeyAction),
    AnimationTick,
    Resize,
}

fn poll_for_key_or_animation(spinner: &Spinner) -> io::Result<PollResult> {
    let timeout = spinner.time_until_next_frame();

    if event::poll(timeout)? {
        match event::read()? {
            Event::Key(key) => {
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                    || key.code == KeyCode::Char('q')
                    || key.code == KeyCode::Char('Q')
                {
                    return Ok(PollResult::KeyPressed(KeyAction::Quit));
                } else if key.code == KeyCode::Char('t') || key.code == KeyCode::Char('T') {
                    return Ok(PollResult::KeyPressed(KeyAction::Tall));
                } else if key.code == KeyCode::Char('+') || key.code == KeyCode::Char('=') {
                    return Ok(PollResult::KeyPressed(KeyAction::IncreaseViewport));
                } else if key.code == KeyCode::Char('-') || key.code == KeyCode::Char('_') {
                    return Ok(PollResult::KeyPressed(KeyAction::DecreaseViewport));
                } else if matches!(key.code, KeyCode::Char(_) | KeyCode::Enter) {
                    return Ok(PollResult::KeyPressed(KeyAction::Normal));
                }
                // Key we don't care about, continue polling
                poll_for_key_or_animation(spinner)
            }
            Event::Resize(_, _) => Ok(PollResult::Resize),
            _ => {
                // Other event we don't care about, continue polling
                poll_for_key_or_animation(spinner)
            }
        }
    } else {
        // Timeout reached, time to animate
        Ok(PollResult::AnimationTick)
    }
}

fn render_viewport(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    spinner: &Spinner,
    viewport_height: u16,
) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let spinner_text = format!(
            " {} Inline Viewport ({} lines) {} ",
            spinner.current_frame(),
            viewport_height,
            spinner.current_frame()
        );
        let block = Block::bordered()
            .title(spinner_text)
            .border_style(Style::default().fg(Color::Cyan));
        frame.render_widget(block, area);
    })?;
    Ok(())
}

fn main() -> Result<()> {
    const INITIAL_HEIGHT: u16 = 3;
    const MIN_HEIGHT: u16 = 2;

    let mut terminal = init_terminal(INITIAL_HEIGHT)?;
    let mut spinner = Spinner::new();
    let mut viewport_height = INITIAL_HEIGHT;

    render_viewport(&mut terminal, &spinner, viewport_height)?;

    let colors = [
        CColor::Green,
        CColor::Yellow,
        CColor::Magenta,
        CColor::Blue,
        CColor::Red,
        CColor::Cyan,
    ];

    let mut line_num = 1;
    let mut scrollback_history = String::new(); // has crlf-s instead of lf-s

    loop {
        match poll_for_key_or_animation(&spinner)? {
            PollResult::AnimationTick => {
                spinner.tick();
                render_viewport(&mut terminal, &spinner, viewport_height)?;
            }
            PollResult::Resize => {
                with_synchronized_output(&mut terminal, |terminal| {
                    resize_purge_rerender(terminal, &scrollback_history)?;
                    render_viewport(terminal, &spinner, viewport_height)?;
                    Ok(())
                })?;
            }
            PollResult::KeyPressed(action) => match action {
                KeyAction::Quit => break,
                KeyAction::IncreaseViewport | KeyAction::DecreaseViewport => {
                    let new_viewport_height = if let KeyAction::IncreaseViewport = action {
                        viewport_height + 1
                    } else {
                        viewport_height.saturating_sub(1)
                    }
                    .clamp(MIN_HEIGHT, terminal.size()?.height.saturating_sub(1));
                    if new_viewport_height != viewport_height {
                        viewport_height = new_viewport_height;
                        with_synchronized_output(&mut terminal, |terminal| {
                            resize_viewport_height(terminal, viewport_height)?;
                            render_viewport(terminal, &spinner, viewport_height)?;
                            Ok(())
                        })?;
                    }
                }
                KeyAction::Tall | KeyAction::Normal => {
                    let area = terminal.size()?;
                    let (terminal_width, terminal_height) =
                        (area.width as usize, area.height as usize);
                    let content = match action {
                        KeyAction::Tall => generate_tall_content(terminal_width, terminal_height),
                        KeyAction::Normal => generate_test_line(line_num, terminal_width),
                        _ => unreachable!(),
                    };
                    let marked_line = build_marked_line(line_num, &content);
                    let colored_content =
                        colorize_content(&marked_line, colors[(line_num - 1) % colors.len()]);

                    // Save to scrollback history, replace LF with CRLF
                    if !scrollback_history.is_empty() {
                        scrollback_history.push_str("\r\n");
                    }
                    scrollback_history.push_str(&colored_content.replace('\n', "\r\n"));

                    // Emit to scrollback and render viewport
                    with_synchronized_output(&mut terminal, |terminal| {
                        emit_to_scrollback(terminal, &colored_content)?;
                        render_viewport(terminal, &spinner, viewport_height)?;
                        Ok(())
                    })?;

                    line_num += 1;
                }
            },
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}
