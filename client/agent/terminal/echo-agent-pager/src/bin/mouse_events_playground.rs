use std::collections::VecDeque;
use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

struct App {
    events: VecDeque<String>,
    last_mouse: Option<String>,
    last_scroll_at: Option<Instant>,
    scroll_intervals: VecDeque<f32>,
}

impl App {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            last_mouse: None,
            last_scroll_at: None,
            scroll_intervals: VecDeque::new(),
        }
    }

    fn push(&mut self, msg: String) {
        self.last_mouse = Some(msg.clone());
        self.events.push_front(msg);
        while self.events.len() > 200 {
            self.events.pop_back();
        }
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.push("started mouse playground".to_string());

    loop {
        terminal.draw(|f| draw(f, &app))?;

        if event::poll(Duration::from_millis(100))? {
            let ev = event::read()?;
            match ev {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => break,
                Event::Mouse(mouse) => {
                    let now = std::time::Instant::now();
                    let is_scroll = matches!(
                        mouse.kind,
                        event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown
                    );
                    let interval_str = if is_scroll {
                        if let Some(prev) = app.last_scroll_at {
                            let ms = now.duration_since(prev).as_secs_f64() * 1000.0;
                            app.scroll_intervals.push_back(ms as f32);
                            while app.scroll_intervals.len() > 20 {
                                app.scroll_intervals.pop_front();
                            }
                            let avg: f32 = app.scroll_intervals.iter().sum::<f32>()
                                / app.scroll_intervals.len() as f32;
                            format!(
                                " dt={ms:.1}ms avg={avg:.1}ms n={}",
                                app.scroll_intervals.len()
                            )
                        } else {
                            app.scroll_intervals.clear();
                            " dt=- (first)".to_string()
                        }
                    } else {
                        if app.last_scroll_at.is_some() {
                            app.scroll_intervals.clear();
                        }
                        String::new()
                    };
                    if is_scroll {
                        app.last_scroll_at = Some(now);
                    }
                    app.push(format!(
                        "mouse {:?} col={} row={} mods={:?}{interval_str}",
                        mouse.kind, mouse.column, mouse.row, mouse.modifiers
                    ));
                }
                Event::Key(key) => {
                    app.push(format!("key {:?}", key));
                }
                other => {
                    app.push(format!("event {:?}", other));
                }
            }
        }
    }

    stdout().execute(DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(8),
        Constraint::Min(1),
    ])
    .split(area);

    let title = Paragraph::new(vec![
        Line::from(Span::styled(
            "mouse-events-playground",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("Esc / Ctrl-Q to quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("info"));
    f.render_widget(title, chunks[0]);

    let last = app.last_mouse.as_deref().unwrap_or("(no mouse event yet)");
    let last_para = Paragraph::new(last).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("last mouse event"),
    );
    f.render_widget(last_para, chunks[1]);

    let sample = vec![
        Line::from(Span::styled(
            "Try dragging across the text below with your trackpad.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(
            "I'm EchoAgent Build, an interactive CLI agent built to help with software engineering tasks.",
        ),
        Line::from(
            "This sample is here so you can drag over real text while watching raw mouse events.",
        ),
        Line::from(""),
        Line::from("Expected useful events: Down(Left), Drag(Left), Up(Left), or at least Moved."),
    ];
    let sample_para = Paragraph::new(sample)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("sample text"));
    f.render_widget(sample_para, chunks[2]);

    let lines: Vec<Line<'static>> = app.events.iter().map(|e| Line::from(e.clone())).collect();
    let log = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("event log"));
    f.render_widget(log, chunks[3]);
}
