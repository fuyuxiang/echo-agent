use std::collections::VecDeque;
use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use echo_agent_pager::scrollback::text_selection::{
    PersistentTextSelection, RangeHit, ResolvedSelectionModel, SelectionEndpoint, SelectionOrigin,
    configured_word_separators, render_persistent_selection_overlay, semantic_selection_at,
    url_range_at_col,
};
use echo_agent_pager::scrollback::{RenderBlock, ScratchBuffer, ScrollbackPane, ScrollbackState};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Maximum time (ms) between consecutive clicks to count as a multi-click.
const MULTI_CLICK_TIMEOUT_MS: u128 = 300;

/// Maximum selected text length shown in event log before truncating.
const MAX_DISPLAY_TEXT_LEN: usize = 60;

/// Playground highlight TTL (`hold`/`word_select` → 0; flash → 500ms for visibility).
fn selection_highlight_duration_ms() -> u64 {
    const PLAYGROUND_FLASH_MS: u64 = 500;
    if echo_agent_pager::appearance::cache::load_keep_text_selection().holds() {
        0
    } else {
        PLAYGROUND_FLASH_MS
    }
}

/// Whether double-click does terminal-like word/line selection (`word_select`)
/// vs. fold toggle, per the unified `keep_text_selection` setting.
fn double_click_action_label() -> &'static str {
    if echo_agent_pager::appearance::cache::load_keep_text_selection().selects_word() {
        "word_select"
    } else {
        "toggle_fold"
    }
}

fn truncate_for_display(text: &str) -> String {
    if text.len() <= MAX_DISPLAY_TEXT_LEN {
        text.to_owned()
    } else {
        let end = text
            .char_indices()
            .take_while(|(i, _)| *i < MAX_DISPLAY_TEXT_LEN)
            .last()
            .map_or(0, |(i, c)| i + c.len_utf8());
        format!("{}...", &text[..end])
    }
}

struct App {
    scrollback: ScrollbackState,
    scratch: ScratchBuffer,
    last_mouse: Option<String>,
    events: VecDeque<String>,
    persistent_selection: Option<PersistentTextSelection>,
    /// Timestamp when the persistent selection was created (for auto-dismiss).
    selection_created_at: Option<Instant>,
    /// Last click for multi-click detection: (time, entry_idx, range_id, block_line_idx, count).
    last_click: Option<(Instant, usize, u16, usize, u8)>,
    /// Stash the last-frame selection model for click processing.
    last_selection_model: ResolvedSelectionModel,
}

impl App {
    fn new() -> Self {
        let mut scrollback = ScrollbackState::new();
        scrollback.push_block(RenderBlock::user_prompt("who are you?"));
        scrollback.push_block(RenderBlock::thinking(
            "I am thinking through how to answer your question before responding.",
        ));
        scrollback.push_block(RenderBlock::agent_message(
            "I'm EchoAgent Build, an interactive CLI agent built to help with software engineering tasks like coding, debugging, refactoring, and exploring codebases.",
        ));
        scrollback.push_block(RenderBlock::agent_message(
            "Try drag-selecting text. Double-click toggles fold by default; set Text selection → Word select for double-click word / triple-click line.",
        ));

        Self {
            scrollback,
            scratch: ScratchBuffer::new(),
            last_mouse: None,
            events: VecDeque::new(),
            persistent_selection: None,
            selection_created_at: None,
            last_click: None,
            last_selection_model: ResolvedSelectionModel::default(),
        }
    }

    fn push(&mut self, msg: String) {
        self.events.push_front(msg);
        while self.events.len() > 200 {
            self.events.pop_back();
        }
    }

    /// Count multi-clicks on the same (entry, range, line).
    fn count_click(&self, now: Instant, hit: &RangeHit) -> u8 {
        if let Some((prev_time, eidx, rid, bli, count)) = self.last_click
            && eidx == hit.entry_idx
            && rid == hit.range_id
            && bli == hit.block_line_idx
            && now.duration_since(prev_time).as_millis() < MULTI_CLICK_TIMEOUT_MS
        {
            count.saturating_add(1)
        } else {
            1
        }
    }

    /// Handle a mouse-up on the scrollback area: detect multi-clicks and
    /// perform word/line selection.
    fn handle_click(&mut self, col: u16, row: u16) {
        let Some(hit) = self.last_selection_model.hit_test_text_exact(col, row) else {
            return;
        };

        let now = Instant::now();
        let click_count = self.count_click(now, &hit);

        match click_count {
            2 => self.select_word(&hit),
            3 => self.select_line(&hit),
            _ => {}
        }

        let next_count = if click_count >= 3 { 0 } else { click_count };
        if next_count > 0 {
            self.last_click = Some((
                now,
                hit.entry_idx,
                hit.range_id,
                hit.block_line_idx,
                next_count,
            ));
        } else {
            self.last_click = None;
        }
    }

    fn select_word(&mut self, hit: &RangeHit) {
        let separators = configured_word_separators();
        let Some(sel) = semantic_selection_at(&self.last_selection_model, hit, separators) else {
            return;
        };
        let kind = if url_range_at_col(&sel.text, 0).is_some() {
            "url"
        } else {
            "word"
        };
        self.push(format!(
            "double-click {kind}: \"{}\"",
            truncate_for_display(&sel.text)
        ));

        self.persistent_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: sel.anchor,
            head: sel.head,
            origin: SelectionOrigin::DoubleClick,
            kind: Default::default(),
        });
        self.selection_created_at = Some(Instant::now());
    }

    fn select_line(&mut self, hit: &RangeHit) {
        let Some(line) = self.last_selection_model.line_for_hit(hit) else {
            return;
        };
        let width = line
            .selectable_cols
            .end
            .saturating_sub(line.selectable_cols.start);
        if width == 0 {
            return;
        }

        self.push(format!(
            "triple-click line: \"{}\"",
            truncate_for_display(&line.text)
        ));

        self.persistent_selection = Some(PersistentTextSelection {
            entry_idx: hit.entry_idx,
            range_id: hit.range_id,
            anchor: SelectionEndpoint {
                block_line_idx: hit.block_line_idx,
                col_within_range: 0,
            },
            head: SelectionEndpoint {
                block_line_idx: hit.block_line_idx,
                col_within_range: width.saturating_sub(1),
            },
            origin: SelectionOrigin::TripleClick,
            kind: Default::default(),
        });
        self.selection_created_at = Some(Instant::now());
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.push("started scrollback selection playground".to_string());

    loop {
        // Auto-dismiss selection highlight after timeout.
        let duration = selection_highlight_duration_ms();
        if duration > 0
            && let Some(created) = app.selection_created_at
            && created.elapsed().as_millis() as u64 >= duration
        {
            app.persistent_selection = None;
            app.selection_created_at = None;
        }

        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            let ev = event::read()?;
            match ev {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    if app.persistent_selection.take().is_some() {
                        app.selection_created_at = None;
                        app.push("Escape: cleared persistent selection".to_string());
                    } else {
                        break;
                    }
                }
                Event::Mouse(mouse) => {
                    let msg = format!(
                        "mouse {:?} col={} row={} mods={:?}",
                        mouse.kind, mouse.column, mouse.row, mouse.modifiers
                    );
                    app.last_mouse = Some(msg.clone());
                    app.push(msg);

                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            app.persistent_selection = None;
                            app.selection_created_at = None;
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            app.handle_click(mouse.column, mouse.row);
                        }
                        _ => {}
                    }
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

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(5),  // info + help
        Constraint::Length(3),  // last mouse event
        Constraint::Length(3),  // selection hit test
        Constraint::Length(3),  // persistent selection state
        Constraint::Length(14), // scrollback
        Constraint::Min(1),     // event log
    ])
    .split(area);

    // -- Info / help panel --
    let dbl_click = double_click_action_label();
    let info = Paragraph::new(vec![
        Line::from(Span::styled(
            "scrollback-selection-playground",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Ctrl-Q quit | Esc clear selection | double_click_action={dbl_click}"
        )),
        Line::from("Double-click: select word/URL | Triple-click: select line"),
    ])
    .block(Block::default().borders(Borders::ALL).title("info"));
    f.render_widget(info, chunks[0]);

    // -- Scrollback render --
    let scrollback_area = chunks[4];
    app.scrollback
        .prepare_layout(scrollback_area.width, scrollback_area.height);

    let mut sb_buf = Buffer::empty(scrollback_area);
    let sb_output = ScrollbackPane::new().active(true).render_with_scratch(
        scrollback_area,
        &mut sb_buf,
        &app.scrollback,
        &mut app.scratch,
    );

    // Stash the selection model for click processing next frame.
    app.last_selection_model = sb_output.selection_model.clone();

    for y in 0..scrollback_area.height {
        for x in 0..scrollback_area.width {
            if let Some(src) = sb_buf.cell((scrollback_area.x + x, scrollback_area.y + y))
                && let Some(dst) = f
                    .buffer_mut()
                    .cell_mut((scrollback_area.x + x, scrollback_area.y + y))
            {
                *dst = src.clone();
            }
        }
    }

    // Render selection overlays.
    if let Some(sel) = &sb_output.selection_box {
        sel.render(f.buffer_mut());
    }
    if let Some(ref ps) = app.persistent_selection {
        render_persistent_selection_overlay(&app.last_selection_model, ps, None, f.buffer_mut());
    }

    // -- Last mouse event panel --
    let last_mouse = app.last_mouse.as_deref().unwrap_or("(no mouse event yet)");
    let last_mouse_para = Paragraph::new(last_mouse).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("last mouse event"),
    );
    f.render_widget(last_mouse_para, chunks[1]);

    // -- Hit test panel --
    let hit = app.last_mouse.as_ref().and_then(|mouse| {
        let parts: Vec<&str> = mouse.split_whitespace().collect();
        let col = parts
            .iter()
            .find_map(|p| p.strip_prefix("col=").and_then(|n| n.parse::<u16>().ok()));
        let row = parts
            .iter()
            .find_map(|p| p.strip_prefix("row=").and_then(|n| n.parse::<u16>().ok()));
        match (col, row) {
            (Some(c), Some(r)) => {
                let exact = sb_output
                    .selection_model
                    .hit_test_text_exact(c, r)
                    .map(|h| {
                        format!(
                            "exact entry={} range={} line={} col={}",
                            h.entry_idx, h.range_id, h.block_line_idx, h.col_within_range
                        )
                    });
                let nearest = sb_output.selection_model.hit_test_selectable_range(c, r);
                Some((exact, nearest))
            }
            _ => None,
        }
    });
    let hit_text = match &hit {
        Some((Some(exact), _)) => exact.clone(),
        Some((None, Some(h))) => format!(
            "nearest entry={} range={} line={} col={} (no exact)",
            h.entry_idx, h.range_id, h.block_line_idx, h.col_within_range
        ),
        Some((None, None)) => format!(
            "no hit (ranges={}, blocks={})",
            sb_output.selection_model.ranges.len(),
            sb_output.selection_model.visible_blocks.len()
        ),
        None => "(no mouse event yet)".to_string(),
    };
    let hit_para = Paragraph::new(hit_text).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("selection hit test"),
    );
    f.render_widget(hit_para, chunks[2]);

    // -- Persistent selection state panel --
    let sel_text = match &app.persistent_selection {
        Some(ps) => {
            let origin = match ps.origin {
                SelectionOrigin::Drag => "Drag",
                SelectionOrigin::DoubleClick => "DoubleClick",
                SelectionOrigin::TripleClick => "TripleClick",
            };
            format!(
                "entry={} range={} anchor=({},{}) head=({},{}) origin={}",
                ps.entry_idx,
                ps.range_id,
                ps.anchor.block_line_idx,
                ps.anchor.col_within_range,
                ps.head.block_line_idx,
                ps.head.col_within_range,
                origin,
            )
        }
        None => "(none)".to_string(),
    };
    let sel_para = Paragraph::new(sel_text).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("persistent selection"),
    );
    f.render_widget(sel_para, chunks[3]);

    // -- Event log --
    let lines: Vec<Line<'static>> = app.events.iter().map(|e| Line::from(e.clone())).collect();
    let log = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("event log"));
    f.render_widget(log, chunks[5]);
}
