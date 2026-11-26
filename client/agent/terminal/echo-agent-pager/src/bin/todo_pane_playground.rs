//! Interactive playground for the Ctrl+T todo pane (hide-done empty state).
//!
//! ```text
//! cargo run -p echo-agent-pager --bin todo-pane-playground
//! ```
//!
//! Keys: h = hide/show done (same as real pane), n/p = scenario, Esc/q = quit.

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use echo_agent_pager::appearance::LayoutConfig;
use echo_agent_pager::views::todo_pane::TodoPane;
use echo_agent_runtime::tools::{TodoItem, TodoPriority, TodoStatus};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

type Scenario = (&'static str, &'static str, Vec<TodoItem>);

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.into(),
        priority: TodoPriority::default(),
        status,
        meta: None,
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        (
            "Repro: 5 done + 1 cancelled (press h)",
            "Default show_done=true lists rows; press h → should NOT say All done.",
            vec![
                item("Wire vim mode into command palette", TodoStatus::Completed),
                item("Wire vim mode into /model picker", TodoStatus::Completed),
                item("Wire vim mode into /theme picker", TodoStatus::Completed),
                item("Wire vim mode into /resume picker", TodoStatus::Completed),
                item("Wire vim mode into sessions modal", TodoStatus::Completed),
                item("Ship misleading All done copy", TodoStatus::Cancelled),
            ],
        ),
        (
            "All completed (press h → All done.)",
            "Hide done with only completed items → All done.",
            vec![
                item("Read Slack feedback", TodoStatus::Completed),
                item("Implement empty_placeholder_message", TodoStatus::Completed),
                item("Add unit tests", TodoStatus::Completed),
            ],
        ),
        (
            "Only cancelled (press h)",
            "No completed rows → N cancelled., never All done.",
            vec![
                item("Abandoned approach A", TodoStatus::Cancelled),
                item("Abandoned approach B", TodoStatus::Cancelled),
            ],
        ),
        (
            "Mixed open work",
            "Pending/in progress stay visible when hide done is on.",
            vec![
                item("Done already", TodoStatus::Completed),
                item("In flight", TodoStatus::InProgress),
                item("Not started", TodoStatus::Pending),
                item("Dropped", TodoStatus::Cancelled),
            ],
        ),
        ("Empty plan", "No todos → No todo items.", vec![]),
    ]
}

struct App {
    scenarios: Vec<Scenario>,
    active: usize,
    pane: TodoPane,
    layout: LayoutConfig,
    status: String,
}

impl App {
    fn new() -> Self {
        let scenarios = scenarios();
        let mut pane = TodoPane::new();
        pane.overlay.show();
        let mut app = Self {
            scenarios,
            active: 0,
            pane,
            layout: LayoutConfig::default(),
            status: String::new(),
        };
        app.apply_scenario();
        app
    }

    fn apply_scenario(&mut self) {
        let items = self.scenarios[self.active].2.clone();
        self.pane.update_todos(items);
        // Match real default: done rows visible until user presses h.
        while !self.pane.show_done() {
            self.pane.toggle_show_done();
        }
        self.pane.overlay.show();
        self.refresh_status();
    }

    fn switch(&mut self, idx: usize) {
        self.active = idx;
        self.apply_scenario();
    }

    fn refresh_status(&mut self) {
        let c = self.pane.counts();
        let (_, hint, _) = &self.scenarios[self.active];
        self.status = format!(
            "{hint}  |  counts: ▶{} □{} ✓{} ✗{}  show_done={}  visible_hint={}",
            c.in_progress,
            c.pending,
            c.completed,
            c.cancelled,
            self.pane.show_done(),
            if self.pane.show_done() {
                "all rows"
            } else {
                "open only (or empty placeholder)"
            },
        );
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != event::KeyEventKind::Press {
            continue;
        }
        if matches!(key.code, KeyCode::Esc)
            || (key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::NONE)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            break;
        }
        if key.code == KeyCode::Char('n') {
            let next = (app.active + 1) % app.scenarios.len();
            app.switch(next);
            continue;
        }
        if key.code == KeyCode::Char('p') {
            let prev = if app.active == 0 {
                app.scenarios.len() - 1
            } else {
                app.active - 1
            };
            app.switch(prev);
            continue;
        }
        // Forward to TodoPane (h, j/k, /, …) then refresh status for show_done.
        let _ = app.pane.handle_key(&key);
        app.refresh_status();
    }

    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let todo_h = app.pane.desired_height(area.height).clamp(3, 12);
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(todo_h),
        Constraint::Length(4),
        Constraint::Min(0),
    ])
    .split(area);

    let (title, _, _) = &app.scenarios[app.active];
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(
                "todo-pane-playground [{}/{}] {title}",
                app.active + 1,
                app.scenarios.len(),
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("h: hide/show done   n/p: scenario   j/k: navigate list   Esc/q: quit"),
        Line::from(
            "Record scenario 1: open → press h → placeholder should show counts, not All done.",
        ),
    ])
    .block(Block::default().borders(Borders::ALL).title("info"));
    f.render_widget(header, chunks[0]);

    let todo_area = chunks[1];
    if todo_area.height > 0 && todo_area.width > 0 {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("todo pane (Ctrl+T)");
        let inner = block.inner(todo_area);
        f.render_widget(block, todo_area);
        app.pane.render(inner, f.buffer_mut(), true, &app.layout);
    }

    let status = Paragraph::new(app.status.clone())
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("status"));
    f.render_widget(status, chunks[2]);
}
