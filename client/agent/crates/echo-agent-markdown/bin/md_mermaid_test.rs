//! Interactive Mermaid diagram rendering playground.
//!
//! Run with:
//!   cargo run -p echo-agent-markdown --features playground --bin md-mermaid-test
//!
//! Controls:
//!   Esc / Tab    — toggle textarea focus
//!   h / Left     — shrink render width  (when unfocused)
//!   l / Right    — grow render width    (when unfocused)
//!   n            — next sample          (when unfocused)
//!   q / ^C / ^D  — quit                 (when unfocused)

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidgetRef, Wrap};

use echo_agent_markdown::{
    MarkdownBuffers, MarkdownStyle, render_markdown_ratatui_with_buffers_width,
};
use echo_agent_ratatui_textarea::{TextArea, TextAreaState};

#[path = "playground_common.rs"]
mod playground_common;
use playground_common::{fg, get_syntect, md_style, rgb_color};

const MD_STYLE: MarkdownStyle = md_style(fg(rgb_color(192, 202, 245)));

const SAMPLES: &[&str] = &[
    "```mermaid\nflowchart TD\n    A[Start] --> B{Is it working?}\n    B -->|Yes| C[Ship it]\n    B -->|No| D[Debug]\n    D --> B\n```\n",
    "```mermaid\ngraph TD\n    A[Client] --> B[Load Balancer]\n    B --> C[Server 1]\n    B --> D[Server 2]\n    C --> E[(Database)]\n    D --> E\n```\n",
    "```mermaid\nflowchart LR\n    A --> B --> C --> D\n```\n",
    "```mermaid\nsequenceDiagram\n    Alice->>Bob: Hello Bob\n    Bob-->>Alice: Hi Alice\n```\n",
    "```mermaid\nsequenceDiagram\n    autonumber\n    participant C as Client\n    participant S as Server\n    participant D as Database\n    C->>S: GET /api/items\n    S->>D: SELECT * FROM items\n    D-->>S: rows\n    S-->>C: 200 OK\n    C->>C: render list\n    Note over C,S: happy path\n    loop retry x3\n        C-x S: timeout\n    end\n```\n",
    "```mermaid\ngraph TD\n  Start --> Stop\n```\n",
    "```mermaid\nstateDiagram-v2\n    [*] --> Idle\n    Idle --> Loading: fetch\n    Loading --> Ready: ok\n    Loading --> Error: fail\n    Error --> Idle: retry\n    Ready --> [*]\n```\n",
    "```mermaid\nflowchart TD\n    A[Read config] & B[Load cache] --> C{Valid?}\n    C -.->|no| D[Rebuild]\n    C ==>|yes| E[Serve]\n    D --o E\n    E -->|poll| E\n```\n",
    "```mermaid\ngraph TD\n    C[ccc]\n    D[ddd]\n    A --> D\n    B --> C\n    D --> P[pp]\n    C --> Q[qq]\n```\n",
    "```mermaid\ngraph TD\n    A --> D[ddd]\n    A --> C[ccc]\n    B --> C\n    B --> D\n```\n",
    "```mermaid\ngraph TD\n    U[User] --> gw\n    subgraph gw [Gateway]\n        LB[load balancer] --> RL[rate limiter]\n    end\n    subgraph core [Services]\n        API[api] --> W[worker]\n        W --> Q[(queue)]\n    end\n    gw --> core\n    core --> DB[(postgres)]\n```\n",
    "```mermaid\nclassDiagram\n    class Animal {\n        <<abstract>>\n        +int age\n        +isMammal() bool\n        +mate()\n    }\n    class Duck {\n        +String beakColor\n        +swim()\n    }\n    Animal <|-- Duck\n    Animal <|-- Fish\n    Duck *-- Bill\n    Duck ..> Pond : swims in\n```\n",
    "```mermaid\nerDiagram\n    CUSTOMER ||--o{ ORDER : places\n    ORDER ||--|{ LINE_ITEM : contains\n    PRODUCT }o..o{ LINE_ITEM : \"is in\"\n    CUSTOMER {\n        string name PK\n        int custNumber\n    }\n    ORDER {\n        int orderNumber\n        date placed\n    }\n```\n",
];

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

struct App {
    textarea: TextArea,
    textarea_state: TextAreaState,
    textarea_focused: bool,
    textarea_area: Rect,
    render_width: usize,
    sample: usize,
    source: String,
    lines: Vec<Line<'static>>,
}

impl App {
    fn new() -> Self {
        let sample = std::env::var("MERMAID_SAMPLE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
            % SAMPLES.len();
        let width = std::env::var("MERMAID_WIDTH")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(70);
        let mut textarea = TextArea::new();
        textarea.set_text(SAMPLES[sample]);
        textarea.show_scrollbar = false;
        let source = SAMPLES[sample].to_string();
        let lines = render_full(&source, width);
        Self {
            textarea,
            textarea_state: TextAreaState::default(),
            textarea_focused: false,
            textarea_area: Rect::default(),
            render_width: width,
            sample,
            source,
            lines,
        }
    }

    fn rerender(&mut self) {
        self.source = self.textarea.text().to_string();
        self.lines = render_full(&self.source, self.render_width);
    }

    fn adjust_width(&mut self, delta: isize) {
        let new_w = (self.render_width as isize + delta).max(10) as usize;
        if new_w != self.render_width {
            self.render_width = new_w;
            self.lines = render_full(&self.source, self.render_width);
        }
    }

    fn next_sample(&mut self) {
        self.sample = (self.sample + 1) % SAMPLES.len();
        self.textarea.set_text(SAMPLES[self.sample]);
        self.rerender();
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            let ev = event::read()?;
            match &ev {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Esc | KeyCode::Tab,
                    ..
                }) if app.textarea_focused => {
                    app.textarea_focused = false;
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                }) if !app.textarea_focused => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c') | KeyCode::Char('d'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) if !app.textarea_focused => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Tab, ..
                }) if !app.textarea_focused => {
                    app.textarea_focused = true;
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('n'),
                    ..
                }) if !app.textarea_focused => app.next_sample(),
                Event::Key(KeyEvent {
                    code: KeyCode::Char('h') | KeyCode::Left,
                    ..
                }) if !app.textarea_focused => app.adjust_width(-2),
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l') | KeyCode::Right,
                    ..
                }) if !app.textarea_focused => app.adjust_width(2),
                Event::Key(key) if app.textarea_focused => {
                    app.textarea.input(*key);
                    app.rerender();
                }
                _ => {}
            }
        }
    }

    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();

    let render_w = app.render_width as u16;
    let render_height = wrapped_line_count(&app.lines, render_w).max(1) + 2;
    let header_height = 2u16;
    let textarea_height = size
        .height
        .saturating_sub(header_height + render_height)
        .max(6);

    let chunks = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Length(textarea_height),
        Constraint::Length(render_height),
    ])
    .split(size);

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
            " width: {} | sample {}/{} ",
            app.render_width,
            app.sample + 1,
            SAMPLES.len()
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
            " Tab: edit | h/l: width | n: next sample | q/^C: quit ",
            Style::default().fg(Color::DarkGray),
        )
    };
    let header = Paragraph::new(vec![
        Line::from(vec![focus_indicator, Span::raw(" "), width_info]),
        Line::from(keys),
    ]);
    f.render_widget(header, chunks[0]);

    let border_color = if app.textarea_focused {
        Color::Green
    } else {
        Color::DarkGray
    };
    let textarea_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            " Mermaid Source ",
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

    let title = format!(" rendered (inner width {}) ", app.render_width);
    render_panel(f, chunks[2], &title, &app.lines, render_w);
}

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
                display_w.div_ceil(w) as u16
            }
        })
        .sum()
}

fn render_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    lines: &[Line<'static>],
    inner_w: u16,
) {
    let outer_w = (inner_w + 2).min(area.width);
    let box_area = Rect {
        x: area.x,
        y: area.y,
        width: outer_w,
        height: area.height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
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
