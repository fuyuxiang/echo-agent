//! Interactive playground for the scrollback search render layer.
//!
//! Drives a real [`ScrollbackSearchState`] over a sample scrollback so the
//! search bar and match highlighting can be eyeballed before the feature is
//! wired into the production input path. Type to search, `Enter` to accept,
//! `n` / `N` to step through matches (which scrolls the match into view via
//! `reveal_entry_line`), `Esc` to clear the query (or quit when already empty),
//! `Ctrl-Q` to quit.

use std::collections::VecDeque;
use std::io::{self, stdout};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use echo_agent_pager::scrollback::{
    RenderBlock, ScratchBuffer, ScrollbackPane, ScrollbackSearchState, ScrollbackState,
};
use echo_agent_pager::theme::Theme;
use echo_agent_pager::views::picker::render_search_bar_with_viewport;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

struct App {
    scrollback: ScrollbackState,
    scratch: ScratchBuffer,
    search: ScrollbackSearchState,
    events: VecDeque<String>,
}

impl App {
    fn new() -> Self {
        let mut scrollback = ScrollbackState::new();
        scrollback.push_block(RenderBlock::user_prompt("how does the search index work?"));
        scrollback.push_block(RenderBlock::thinking(
            "The search index caches each entry's source text and scans it for query matches.",
        ));
        scrollback.push_block(RenderBlock::agent_message(
            "The scrollback search index keeps one owned String per entry and re-syncs only when \
             content changes. The search bar shows the live query, and every match on a visible \
             row is highlighted. Search again to find more matches.",
        ));
        scrollback.push_block(RenderBlock::agent_message(
            "Press n and N to step through matches; the current match is scrolled into view.",
        ));

        Self {
            scrollback,
            scratch: ScratchBuffer::new(),
            search: ScrollbackSearchState::open(),
            events: VecDeque::new(),
        }
    }

    fn push(&mut self, msg: String) {
        self.events.push_front(msg);
        while self.events.len() > 200 {
            self.events.pop_back();
        }
    }

    fn log_query(&mut self) {
        self.push(format!(
            "query={:?} matches={}",
            self.search.query(),
            self.search.match_count()
        ));
    }

    /// Scroll the current match into view (exercises the reveal path).
    fn reveal_current(&mut self) {
        if let Some(m) = self.search.current()
            && let Some(idx) = self.scrollback.index_of_id(m.entry_id)
        {
            let line = m.line_in_entry;
            self.scrollback.reveal_entry_line(idx, line);
        }
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.push("started scrollback search playground".to_string());

    loop {
        // Matching runs on a background thread; pick up results each iteration
        // and scroll the freshly parked match into view, mirroring the per-tick
        // poll in the production app.
        if app.search.poll() {
            app.reveal_current();
        }

        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => break,
                Event::Key(key) => {
                    if handle_key(&mut app, key) {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    stdout().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

/// Returns `true` when the app should quit.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // Esc clears the query; quit only when it's already empty.
        KeyCode::Esc => {
            if app.search.query().is_empty() {
                return true;
            }
            app.search = ScrollbackSearchState::open();
            app.push("clear".to_string());
        }
        KeyCode::Enter => {
            app.search.accept();
            app.push("accept: browsing".to_string());
        }
        // While browsing (accepted), `n` / `N` navigate. While composing they
        // are typed into the query, matching real vim `/` behavior.
        KeyCode::Char('n') if !app.search.is_composing() => {
            app.search.next();
            app.reveal_current();
            app.push(format!("next -> {:?}", app.search.current_index()));
        }
        KeyCode::Char('N') if !app.search.is_composing() => {
            app.search.prev();
            app.reveal_current();
            app.push(format!("prev -> {:?}", app.search.current_index()));
        }
        _ if app.search.is_composing() => {
            let before = app.search.query().to_owned();
            if app.search.handle_query_key(&key, &app.scrollback) && app.search.query() != before {
                app.log_query();
            }
        }
        _ => {}
    }
    false
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let theme = Theme::current();
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(4),  // info / help
        Constraint::Length(14), // scrollback + search bar
        Constraint::Min(1),     // event log
    ])
    .split(area);

    // -- Info / help panel --
    let info = Paragraph::new(vec![
        Line::from(Span::styled(
            "scrollback-search-playground",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(
            "Type to search | Enter accept | n/N navigate (after Enter) | Esc clear/quit | Ctrl-Q quit",
        ),
    ])
    .block(Block::default().borders(Borders::ALL).title("info"));
    f.render_widget(info, chunks[0]);

    // -- Scrollback + search bar --
    // Reserve the bottom row of the block for the search bar, exactly as the
    // production draw path will when `scrollback_search.is_some()`.
    let block_area = chunks[1];
    let sb_area = Rect {
        height: block_area.height.saturating_sub(1),
        ..block_area
    };
    let bar_y = sb_area.y + sb_area.height;

    app.scrollback.prepare_layout(sb_area.width, sb_area.height);

    // Highlight only when the query compiles to a usable regex.
    let highlight = app.search.highlight_regex();

    let mut sb_buf = Buffer::empty(block_area);
    let _ = ScrollbackPane::new()
        .active(true)
        .with_search_highlight(highlight)
        .render_with_scratch(sb_area, &mut sb_buf, &app.scrollback, &mut app.scratch);

    for y in 0..sb_area.height {
        for x in 0..sb_area.width {
            if let Some(src) = sb_buf.cell((sb_area.x + x, sb_area.y + y))
                && let Some(dst) = f.buffer_mut().cell_mut((sb_area.x + x, sb_area.y + y))
            {
                *dst = src.clone();
            }
        }
    }

    // -- Search bar --
    let query = app.search.query();
    let counter = match app.search.current_index() {
        Some(i) => Some(format!("{}/{}", i + 1, app.search.match_count())),
        None if app.search.has_error() => Some("bad pattern".to_string()),
        None if !query.is_empty() => Some("no matches".to_string()),
        None => None,
    };
    let counter_width = counter
        .as_deref()
        .map_or(0, |text| UnicodeWidthStr::width(text) as u16);
    let search_layout =
        echo_agent_pager::views::picker::search_bar_layout(block_area.width, counter_width);
    render_search_bar_with_viewport(
        f.buffer_mut(),
        block_area.x,
        bar_y,
        search_layout,
        &theme,
        query,
        app.search.is_composing(),
        query.is_empty() && app.search.is_composing(),
        None,
        app.search.query_viewport(search_layout.input_width()),
    );

    // Right-aligned match counter: `m/n`, or `no matches` for a live query.
    if let Some(counter) = counter
        && search_layout.trailing_width() > 0
    {
        let w = counter.width() as u16;
        if block_area.width > w {
            f.buffer_mut().set_string(
                block_area.x + block_area.width - w,
                bar_y,
                &counter,
                Style::default().fg(theme.gray),
            );
        }
    }

    // -- Event log --
    let lines: Vec<Line<'static>> = app.events.iter().map(|e| Line::from(e.clone())).collect();
    let log = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("event log"));
    f.render_widget(log, chunks[2]);
}
