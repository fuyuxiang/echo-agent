use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use echo_agent_pager::theme::Theme;
use echo_agent_pager::views::prompt_widget::StashedPrompt;
use echo_agent_pager::views::question_view::{
    QUESTION_VIEW_HPAD, QuestionViewState, question_view_height, render_question_view,
};
use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
    Question, QuestionOption,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Hardcoded example question sets for UI playground scenarios.
fn example_scenarios() -> Vec<(&'static str, Vec<Question>)> {
    vec![
        (
            "Commit confirmation (preview with multi-line message)",
            vec![Question {
                question: "Ready to commit the staged changes with this conventional commit message?".into(),
                options: vec![
                    QuestionOption {
                        label: "Yes, commit now".into(),
                        description: "Run git commit with the message below and push to origin".into(),
                        preview: Some(
                            "fix(example-skills): resolve post-setup review findings\n\n\
                             - Move path resolution before vendor existence check (HIGH)\n\
                             - Improve awk parser to skip -b/-B args (MEDIUM)\n\n\
                             Addresses review-bot inline comments on PR #1001."
                                .into(),
                        ),
                        id: None,
                    },
                    QuestionOption {
                        label: "Edit message first".into(),
                        description: "Provide a different commit message".into(),
                        preview: None,
                        id: None,
                    },
                    QuestionOption {
                        label: "Cancel".into(),
                        description: "Do not commit yet".into(),
                        preview: None,
                        id: None,
                    },
                ],
                multi_select: None,
                            id: None,
            }],
        ),
        (
            "Commit message choice (preview on multiple options)",
            vec![Question {
                question: "What commit message should I use? (I'll stage + commit + push after your confirmation)".into(),
                options: vec![
                    QuestionOption {
                        label: "Use my suggested one (with PROJ-1234)".into(),
                        description: "fix(example-skills): resolve post-setup review findings (PROJ-1234)".into(),
                        preview: Some(
                            "fix(example-skills): resolve post-setup review findings\n\n\
                             - Move path resolution before vendor existence check (HIGH review feedback)\n\
                             - Improve awk parser to skip -b/-B args (MEDIUM)\n\n\
                             Addresses inline comments on PR #1001 for PROJ-1234."
                                .into(),
                        ),
                        id: None,
                    },
                    QuestionOption {
                        label: "Simpler: fix post-setup.sh per review".into(),
                        description: "Just a short one".into(),
                        preview: None,
                        id: None,
                    },
                    QuestionOption {
                        label: "Provide custom message".into(),
                        description: "I'll type the full message".into(),
                        preview: None,
                        id: None,
                    },
                ],
                multi_select: None,
                            id: None,
            }],
        ),
        (
            "Multi-select with previews",
            vec![Question {
                question: "Which database engines should we evaluate?\n\nSelect all that apply for the backend service.".into(),
                options: vec![
                    QuestionOption {
                        label: "PostgreSQL (Recommended)".into(),
                        description: "Battle-tested relational DB with JSONB support".into(),
                        preview: Some("CREATE TABLE users (\n  id SERIAL PRIMARY KEY,\n  email TEXT UNIQUE NOT NULL\n);".into()),
                        id: None,
                    },
                    QuestionOption {
                        label: "Redis".into(),
                        description: "In-memory key-value store for caching".into(),
                        preview: Some("SET user:1 '{\"email\":\"a@b.com\"}' EX 3600".into()),
                                            id: None,
                    },
                    QuestionOption {
                        label: "Cassandra".into(),
                        description: "Wide-column store for large datasets".into(),
                        preview: None,
                        id: None,
                    },
                ],
                multi_select: Some(true),
                            id: None,
            }],
        ),
        (
            "Multi-tab: architecture decisions (Tab/Shift-Tab to switch)",
            vec![
                Question {
                    question: "Which database engine should we use for the backend?".into(),
                    options: vec![
                        QuestionOption {
                            label: "PostgreSQL (Recommended)".into(),
                            description: "Battle-tested relational DB with JSONB".into(),
                            preview: Some(
                                "CREATE TABLE users (\n  id SERIAL PRIMARY KEY,\n  email TEXT UNIQUE NOT NULL,\n  created_at TIMESTAMPTZ DEFAULT NOW()\n);".into(),
                            ),
                            id: None,
                        },
                        QuestionOption {
                            label: "SQLite".into(),
                            description: "Embedded, zero-config, single-file".into(),
                            preview: None,
                            id: None,
                        },
                        QuestionOption {
                            label: "Cassandra".into(),
                            description: "Wide-column store for large datasets".into(),
                            preview: None,
                            id: None,
                        },
                    ],
                    multi_select: None,
                                    id: None,
                },
                Question {
                    question: "Which caching strategy do you want?".into(),
                    options: vec![
                        QuestionOption {
                            label: "Redis".into(),
                            description: "In-memory key-value store, distributed".into(),
                            preview: Some("SET session:abc123 '{\"user_id\": 42}' EX 3600".into()),
                                                    id: None,
                        },
                        QuestionOption {
                            label: "In-process LRU".into(),
                            description: "No external dependency, per-instance cache".into(),
                            preview: None,
                            id: None,
                        },
                    ],
                    multi_select: None,
                                    id: None,
                },
                Question {
                    question: "Which features should be enabled at launch?\n\nSelect all that apply.".into(),
                    options: vec![
                        QuestionOption {
                            label: "Auth".into(),
                            description: "JWT-based authentication middleware".into(),
                            preview: None,
                            id: None,
                        },
                        QuestionOption {
                            label: "Rate limiting".into(),
                            description: "Token bucket per API key".into(),
                            preview: None,
                            id: None,
                        },
                        QuestionOption {
                            label: "Audit logging".into(),
                            description: "Structured logs for compliance".into(),
                            preview: None,
                            id: None,
                        },
                        QuestionOption {
                            label: "Metrics".into(),
                            description: "Prometheus /metrics endpoint".into(),
                            preview: None,
                            id: None,
                        },
                    ],
                    multi_select: Some(true),
                                    id: None,
                },
            ],
        ),
    ]
}

struct App {
    scenarios: Vec<(&'static str, Vec<Question>)>,
    active_scenario: usize,
    state: QuestionViewState,
    theme: Theme,
    status: String,
}

impl App {
    fn new() -> Self {
        let scenarios = example_scenarios();
        let state = QuestionViewState::new(
            "playground".into(),
            scenarios[0].1.clone(),
            StashedPrompt::default(),
        );
        Self {
            scenarios,
            active_scenario: 0,
            state,
            theme: Theme::default(),
            status: "j/k navigate, Space/Enter select, n/p switch scenario, Esc quit".into(),
        }
    }

    fn switch_scenario(&mut self, idx: usize) {
        self.active_scenario = idx;
        self.state = QuestionViewState::new(
            "playground".into(),
            self.scenarios[idx].1.clone(),
            StashedPrompt::default(),
        );
        self.status = format!(
            "Scenario {}/{}: {}",
            idx + 1,
            self.scenarios.len(),
            self.scenarios[idx].0
        );
    }
}

fn format_cursor_status(state: &QuestionViewState) -> String {
    let preview_str = state
        .focused_preview()
        .map(|p| {
            let trunc: String = p.chars().take(40).collect();
            format!("{trunc}...")
        })
        .unwrap_or_else(|| "None".into());
    format!("cursor={} preview={}", state.cursor(), preview_str)
}

fn format_tab_status(state: &QuestionViewState) -> String {
    let question_text = state
        .questions
        .get(state.active_tab)
        .map(|q| q.question.as_str())
        .unwrap_or("?");
    format!(
        "tab {}/{} - {}",
        state.active_tab + 1,
        state.questions.len(),
        question_text,
    )
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.switch_scenario(0);

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
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

                Event::Key(KeyEvent {
                    code: KeyCode::Char('j') | KeyCode::Down,
                    ..
                }) => {
                    let cur = app.state.cursor();
                    app.state.set_cursor(cur + 1);
                    app.status = format_cursor_status(&app.state);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('k') | KeyCode::Up,
                    ..
                }) => {
                    let cur = app.state.cursor();
                    app.state.set_cursor(cur.saturating_sub(1));
                    app.status = format_cursor_status(&app.state);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char(' ') | KeyCode::Enter,
                    ..
                }) => {
                    let tab = app.state.active_tab;
                    let cur = app.state.cursor();
                    app.state.toggle_option(tab, cur);
                    app.status = format!(
                        "toggled option {} -> selected: {:?}",
                        cur,
                        app.state.selected_labels(tab)
                    );
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Tab,
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => {
                    app.state.next_question();
                    app.status = format_tab_status(&app.state);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::BackTab,
                    ..
                }) => {
                    app.state.prev_question();
                    app.status = format_tab_status(&app.state);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('n'),
                    ..
                }) => {
                    let next = (app.active_scenario + 1) % app.scenarios.len();
                    app.switch_scenario(next);
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('p'),
                    ..
                }) => {
                    let prev = if app.active_scenario == 0 {
                        app.scenarios.len() - 1
                    } else {
                        app.active_scenario - 1
                    };
                    app.switch_scenario(prev);
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
    let area = f.area();

    // Layout: header(3) + question view (dynamic) + status(3)
    let content_w = area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
    let qv_height = question_view_height(&mut app.state, area.height.saturating_sub(6), content_w);

    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(qv_height),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(area);

    // Header
    let scenario_label = format!(
        "question-view-playground [{}/{}] {}",
        app.active_scenario + 1,
        app.scenarios.len(),
        app.scenarios[app.active_scenario].0,
    );
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            scenario_label,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("j/k: navigate  Space/Enter: select  Tab/Shift-Tab: switch tab  n/p: scenario  Esc: quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("info"));
    f.render_widget(header, chunks[0]);

    // Question view
    let qv_area = chunks[1];
    if qv_area.height > 0 && qv_area.width > 0 {
        render_question_view(f.buffer_mut(), qv_area, &app.state, None, &app.theme, true);
    }

    // Status bar
    let status = Paragraph::new(app.status.clone())
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("status"));
    f.render_widget(status, chunks[2]);
}
