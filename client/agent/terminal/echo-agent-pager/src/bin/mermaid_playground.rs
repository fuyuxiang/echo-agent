//! Interactive playground for mermaid diagrams in the real scrollback path.
//!
//! Builds a [`ScrollbackState`] containing an agent message whose markdown holds
//! a mermaid code block, then renders it through the production
//! [`ScrollbackPane`] so the diagram can be eyeballed exactly as the TUI draws
//! it (including word-wrapping). Pick a sample with `MERMAID_SAMPLE=<n>`.
//!
//! Controls: arrows / PageUp / PageDown scroll, `q` / `Esc` / `Ctrl-Q` quit.

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use echo_agent_pager::scrollback::{RenderBlock, ScratchBuffer, ScrollbackPane, ScrollbackState};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

const SAMPLES: &[(&str, &str)] = &[
    (
        "draw a flowchart for the deploy decision",
        "Here is the deploy flow:\n\n```mermaid\nflowchart TD\n    A[Start] --> B{Is it working?}\n    B -->|Yes| C[Ship it]\n    B -->|No| D[Debug]\n    D --> B\n```\n\nLet me know if you want changes.",
    ),
    (
        "show the request path",
        "Sure:\n\n```mermaid\ngraph TD\n    A[Client] --> B[Load Balancer]\n    B --> C[Server 1]\n    B --> D[Server 2]\n    C --> E[(Database)]\n    D --> E\n```\n",
    ),
    (
        "a simple pipeline left to right",
        "```mermaid\nflowchart LR\n    A[Ingest] --> B[Transform] --> C[Validate] --> D[Store]\n```\n",
    ),
    (
        "sequence diagram please",
        "```mermaid\nsequenceDiagram\n    Alice->>Bob: Hello Bob\n    Bob-->>Alice: Hi Alice\n```\n",
    ),
    (
        "coffee-making decision flowchart",
        "Here's a coffee-making decision process:\n\n```mermaid\nflowchart TD\n    A[Wake up] --> B{Need caffeine?}\n    B -->|Yes| C[Go to kitchen]\n    B -->|No| D[Drink water]\n    C --> E{Beans available?}\n    D --> F[Stay hydrated]\n    E -->|Yes| G[Grind beans]\n    E -->|No| H[Use instant coffee]\n    G --> I[Brew espresso]\n    H --> J[Boil water]\n    I --> K{Add milk?}\n    J --> K\n    K -->|Yes| L[Make latte]\n    K -->|No| M[Drink black]\n    L --> N[Enjoy coffee]\n    M --> N\n    N --> O[Start the day]\n```\n",
    ),
];

fn main() -> io::Result<()> {
    let sample = std::env::var("MERMAID_SAMPLE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        % SAMPLES.len();
    let (prompt, answer) = SAMPLES[sample];

    let mut scrollback = ScrollbackState::new();
    scrollback.push_block(RenderBlock::user_prompt(prompt));
    scrollback.push_block(RenderBlock::agent_message(answer));
    let mut scratch = ScratchBuffer::new();

    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut first = true;
    loop {
        terminal.draw(|f| {
            let area = f.area();
            scrollback.prepare_layout(area.width, area.height);
            if first {
                scrollback.scroll_to_entry_top(0);
                first = false;
            }
            let mut buf = ratatui::buffer::Buffer::empty(area);
            let _ = ScrollbackPane::new().active(true).render_with_scratch(
                Rect::new(area.x, area.y, area.width, area.height),
                &mut buf,
                &scrollback,
                &mut scratch,
            );
            for y in 0..area.height {
                for x in 0..area.width {
                    if let Some(src) = buf.cell((area.x + x, area.y + y))
                        && let Some(dst) = f.buffer_mut().cell_mut((area.x + x, area.y + y))
                    {
                        *dst = src.clone();
                    }
                }
            }
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
        {
            match code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Up => scrollback.scroll_up(2),
                KeyCode::Down => scrollback.scroll_down(2),
                KeyCode::PageUp => scrollback.scroll_up(15),
                KeyCode::PageDown => scrollback.scroll_down(15),
                _ => {}
            }
        }
    }

    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}
