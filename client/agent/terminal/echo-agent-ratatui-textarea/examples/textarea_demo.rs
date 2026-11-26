//! Demo for TextArea with @-file-search completion and atomic text elements.
//!
//! Run with: cargo run -p echo-agent-ratatui-textarea --example textarea_demo
//!
//! Features demonstrated:
//! - Type `@` to trigger fuzzy file search (real files from current directory)
//! - Tab or Enter confirms selection → creates an atomic text element
//! - Up/Down to navigate results, Esc to dismiss
//! - Bracketed paste → creates paste elements
//! - Elements render as styled chips, cursor skips over them atomically
//! - Display projection: cursor column accounts for display width, not buffer width

use std::collections::HashMap;
use std::io::{self, stdout};
use std::ops::{Range, RangeInclusive};
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::cursor::{EnableBlinking, SetCursorStyle};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidgetRef, Widget};

use echo_agent_ratatui_textarea::wrapping::{RtOptions, word_wrap_line};
use echo_agent_ratatui_textarea::{
    ClipboardProvider, ElementId, ElementKind, MouseAction, TextArea, TextAreaState, TextElement,
    TextElementEventKind,
};

// ── Element kinds ──

const KIND_PASTE: ElementKind = ElementKind(1);
const KIND_FILE_REF: ElementKind = ElementKind(2);

/// Maximum number of file search results shown in the dropdown.
const MAX_RESULTS: usize = 8;

// ── System clipboard provider ──

/// Clipboard backed by `arboard` — copies/pastes to/from system clipboard.
#[derive(Debug)]
struct ArboardClipboard;

impl ClipboardProvider for ArboardClipboard {
    fn get(&mut self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    fn set(&mut self, text: &str) {
        if let Ok(mut clip) = arboard::Clipboard::new() {
            let _ = clip.set_text(text);
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// File search
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// A single fuzzy-matched file result.
struct SearchResult {
    path: String,
    score: i64,
    /// Character indices in `path` that matched the query (for highlighting).
    indices: Vec<usize>,
}

/// Manages the file list, fuzzy matcher, and dropdown state for @-completion.
struct FileSearch {
    all_files: Vec<String>,
    matcher: SkimMatcherV2,
    results: Vec<SearchResult>,
    selected: usize,
}

/// Context extracted from textarea buffer describing an active @-completion trigger.
struct FileSearchContext {
    /// Byte range in the buffer covering `@query` (includes the `@`).
    range: Range<usize>,
    /// The query text (characters after `@`, up to cursor position).
    query: String,
}

impl FileSearch {
    fn new() -> Self {
        let all_files = collect_files();
        Self {
            all_files,
            matcher: SkimMatcherV2::default(),
            results: Vec::new(),
            selected: 0,
        }
    }

    /// Re-run fuzzy matching against `query` and update the results list.
    fn update(&mut self, query: &str) {
        self.results.clear();
        if query.is_empty() {
            // Show first N files alphabetically when query is empty.
            for path in self.all_files.iter().take(MAX_RESULTS) {
                self.results.push(SearchResult {
                    path: path.clone(),
                    score: 0,
                    indices: Vec::new(),
                });
            }
        } else {
            let mut scored: Vec<_> = self
                .all_files
                .iter()
                .filter_map(|path| {
                    self.matcher
                        .fuzzy_indices(path, query)
                        .map(|(score, indices)| SearchResult {
                            path: path.clone(),
                            score,
                            indices,
                        })
                })
                .collect();
            scored.sort_by_key(|b| std::cmp::Reverse(b.score));
            scored.truncate(MAX_RESULTS);
            self.results = scored;
        }
        self.clamp_selection();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    fn selected_path(&self) -> Option<&str> {
        self.results.get(self.selected).map(|r| r.path.as_str())
    }

    fn is_visible(&self) -> bool {
        !self.results.is_empty()
    }

    fn clear(&mut self) {
        self.results.clear();
        self.selected = 0;
    }

    fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.results.len() - 1);
        }
    }

    /// Height needed for the dropdown (0 when hidden).
    fn dropdown_height(&self) -> u16 {
        if self.results.is_empty() {
            0
        } else {
            // results + 2 for the border
            (self.results.len() as u16 + 2).min(MAX_RESULTS as u16 + 2)
        }
    }
}

/// Walk the current directory using the `ignore` crate (respects .gitignore).
fn collect_files() -> Vec<String> {
    let mut files = Vec::new();
    for entry in ignore::Walk::new(".") {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
            continue;
        }
        let path = entry.path().display().to_string();
        let path = path.strip_prefix("./").unwrap_or(&path).to_string();
        files.push(path);
    }
    files.sort();
    files
}

/// Compute the @-completion context from current textarea state.
///
/// Scans backward from `cursor` looking for an `@` that could be a file search
/// trigger. Returns `None` if no valid context is found.
fn compute_file_search_context(
    text: &str,
    cursor: usize,
    elements: &[TextElement],
) -> Option<FileSearchContext> {
    if cursor == 0 {
        return None;
    }

    let at_idx = text[..cursor].rfind('@')?;

    // Don't trigger if the @ is inside an existing element (already confirmed).
    if elements
        .iter()
        .any(|e| at_idx >= e.range.start && at_idx < e.range.end)
    {
        return None;
    }

    // Don't trigger if preceded by alphanumeric or _ (e.g. email-like `user@`).
    if let Some(ch) = text[..at_idx].chars().next_back()
        && (ch.is_alphanumeric() || ch == '_')
    {
        return None;
    }

    // Find the end of the @-token (whitespace or punctuation terminates).
    let token_end = text[at_idx + 1..]
        .char_indices()
        .find_map(|(offset, ch)| {
            (ch.is_whitespace() || matches!(ch, ',' | ';')).then_some(at_idx + 1 + offset)
        })
        .unwrap_or(text.len());

    // Cursor must be within the token.
    if cursor > token_end {
        return None;
    }

    let query = text[at_idx + 1..cursor].to_owned();
    Some(FileSearchContext {
        range: at_idx..token_end,
        query,
    })
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Line select mode (file preview + line range picking)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Clone, Copy, PartialEq)]
enum SelectionState {
    /// No selection active.
    None,
    /// First `v`: anchor line (0-indexed). Range extends as cursor moves.
    Selecting(usize),
    /// Second `v`: range locked (0-indexed, inclusive, sorted).
    Locked(usize, usize),
}

/// Modal state for the file preview / line-range picker.
struct LineSelectMode {
    file_path: String,
    lines: Vec<String>,
    cursor_line: usize, // 0-indexed
    scroll_top: usize,  // 0-indexed first visible line
    viewport_height: usize,
    goto_buf: String,
    selection: SelectionState,
    element_id: ElementId,
}

impl LineSelectMode {
    /// Load a file and create a new line-select session.
    fn open(file_path: String, element_id: ElementId) -> Option<Self> {
        let content = std::fs::read_to_string(&file_path).ok()?;
        let lines: Vec<String> = content.lines().map(String::from).collect();
        if lines.is_empty() {
            return None;
        }
        Some(Self {
            file_path,
            lines,
            cursor_line: 0,
            scroll_top: 0,
            viewport_height: 20,
            goto_buf: String::new(),
            selection: SelectionState::None,
            element_id,
        })
    }

    fn total_lines(&self) -> usize {
        self.lines.len()
    }

    fn move_cursor(&mut self, delta: isize) {
        let max = self.total_lines().saturating_sub(1) as isize;
        self.cursor_line = (self.cursor_line as isize + delta).clamp(0, max) as usize;
        self.ensure_visible();
    }

    fn goto_line(&mut self, line_1indexed: usize) {
        self.cursor_line = line_1indexed
            .saturating_sub(1)
            .min(self.total_lines().saturating_sub(1));
        self.center_cursor();
    }

    fn ensure_visible(&mut self) {
        if self.cursor_line < self.scroll_top {
            self.scroll_top = self.cursor_line;
        } else if self.cursor_line >= self.scroll_top + self.viewport_height {
            self.scroll_top = self.cursor_line + 1 - self.viewport_height;
        }
    }

    fn center_cursor(&mut self) {
        let half = self.viewport_height / 2;
        self.scroll_top = self.cursor_line.saturating_sub(half);
        let max_scroll = self.total_lines().saturating_sub(self.viewport_height);
        self.scroll_top = self.scroll_top.min(max_scroll);
    }

    fn toggle_selection(&mut self) -> SelectionState {
        let prev = self.selection;
        self.selection = match self.selection {
            SelectionState::None => SelectionState::Selecting(self.cursor_line),
            SelectionState::Selecting(anchor) => {
                let (s, e) = sorted(anchor, self.cursor_line);
                SelectionState::Locked(s, e)
            }
            SelectionState::Locked(_, _) => SelectionState::Selecting(self.cursor_line),
        };
        prev
    }

    /// Get the current effective line range (1-indexed, inclusive).
    fn effective_range(&self) -> Option<RangeInclusive<usize>> {
        match self.selection {
            SelectionState::None => None,
            SelectionState::Selecting(anchor) => {
                let (s, e) = sorted(anchor, self.cursor_line);
                Some((s + 1)..=(e + 1))
            }
            SelectionState::Locked(s, e) => Some((s + 1)..=(e + 1)),
        }
    }

    /// Check if a 0-indexed line is in the current selection.
    fn is_selected(&self, line: usize) -> bool {
        match self.selection {
            SelectionState::None => false,
            SelectionState::Selecting(anchor) => {
                let (s, e) = sorted(anchor, self.cursor_line);
                line >= s && line <= e
            }
            SelectionState::Locked(s, e) => line >= s && line <= e,
        }
    }

    /// If the selection covers every line, clear it (whole file = no range).
    fn check_select_all(&mut self) {
        let total = self.total_lines();
        let covers_all = match self.selection {
            SelectionState::Selecting(anchor) => {
                let (s, e) = sorted(anchor, self.cursor_line);
                s == 0 && e + 1 >= total
            }
            SelectionState::Locked(s, e) => s == 0 && e + 1 >= total,
            SelectionState::None => false,
        };
        if covers_all {
            self.selection = SelectionState::None;
        }
    }
}

fn sorted(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

// ── File-ref helpers ──

/// Parse element text like `@foo.rs:123-456` into (path, optional range).
fn parse_file_ref(element_text: &str) -> (&str, Option<RangeInclusive<usize>>) {
    let text = element_text.strip_prefix('@').unwrap_or(element_text);
    if let Some(colon) = text.rfind(':') {
        let suffix = &text[colon + 1..];
        if let Some(range) = parse_line_range_str(suffix) {
            return (&text[..colon], Some(range));
        }
    }
    (text, None)
}

fn parse_line_range_str(s: &str) -> Option<RangeInclusive<usize>> {
    if let Some(dash) = s.find('-') {
        let start: usize = s[..dash].parse().ok()?;
        let end: usize = s[dash + 1..].parse().ok()?;
        Some(start..=end)
    } else {
        let line: usize = s.parse().ok()?;
        Some(line..=line)
    }
}

fn build_file_ref_text(path: &str, range: Option<&RangeInclusive<usize>>) -> String {
    match range {
        None => format!("@{path}"),
        Some(r) if r.start() == r.end() => format!("@{path}:{}", r.start()),
        Some(r) => format!("@{path}:{}-{}", r.start(), r.end()),
    }
}

fn build_file_ref_display(path: &str, range: Option<&RangeInclusive<usize>>) -> Line<'static> {
    let bg = Color::Rgb(30, 50, 30);
    let mut spans = vec![
        Span::styled("@", Style::default().fg(Color::Green).bg(bg).bold()),
        Span::styled(path.to_string(), Style::default().fg(Color::Green).bg(bg)),
    ];
    if let Some(r) = range {
        let range_text = if r.start() == r.end() {
            format!(":{}", r.start())
        } else {
            format!(":{}‑{}", r.start(), r.end())
        };
        spans.push(Span::styled(
            range_text,
            Style::default().fg(Color::Rgb(230, 120, 100)).bg(bg),
        ));
    }
    Line::from(spans)
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// App
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Result of processing an input event.
enum EventResult {
    /// Continue running, redraw the UI.
    Redraw,
    /// Continue running, nothing changed — skip redraw (lets cursor blink).
    Unchanged,
    /// Exit the application.
    Quit,
}

/// Host-side metadata for an element.
struct ElementMeta {
    description: String,
}

struct DemoApp {
    textarea: TextArea,
    textarea_state: TextAreaState,
    element_meta: HashMap<ElementId, ElementMeta>,
    status: String,
    file_search: FileSearch,
    /// Whether the file-search dropdown is logically active.
    fs_active: bool,
    /// Modal line-select / file-preview mode.
    line_select: Option<LineSelectMode>,
    /// Last render area for the textarea (needed for mouse→buffer mapping).
    textarea_area: Rect,
}

impl DemoApp {
    fn new() -> Self {
        let file_search = FileSearch::new();
        let file_count = file_search.all_files.len();
        let mut textarea = TextArea::new();
        textarea.set_clipboard_provider(Box::new(ArboardClipboard));
        Self {
            textarea,
            textarea_state: TextAreaState::default(),
            element_meta: HashMap::new(),
            status: format!(
                "Type text. @ to search {file_count} files. Tab/Enter confirm. Esc quit."
            ),
            file_search,
            fs_active: false,
            line_select: None,
            textarea_area: Rect::default(),
        }
    }

    // ── Event handling ──

    fn handle_event(&mut self, event: Event) -> EventResult {
        match event {
            Event::Paste(text) => {
                self.handle_paste(&text);
                self.recompute_file_search();
                EventResult::Redraw
            }
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Resize(_, _) => EventResult::Redraw,
            _ => EventResult::Unchanged,
        }
    }

    fn handle_paste(&mut self, text: &str) {
        let has_newline = text.contains('\n');

        if !has_newline {
            // Single-line paste → insert inline as plain text (single undo step).
            self.textarea.insert_str(text);
            let char_count = text.chars().count();
            self.status = format!("Pasted {char_count} chars inline");
            return;
        }

        // Multi-line paste → create an element with summary display.
        let line_count = text.lines().count();

        let bg = Color::Rgb(40, 40, 50);
        let display = Line::from(vec![
            Span::styled("[", Style::default().fg(Color::DarkGray).bg(bg)),
            Span::styled(
                format!(
                    "Pasted {} line{}",
                    line_count,
                    if line_count != 1 { "s" } else { "" },
                ),
                Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg),
            ),
            Span::styled("]", Style::default().fg(Color::DarkGray).bg(bg)),
        ]);

        let id = self
            .textarea
            .insert_element(text, KIND_PASTE, Some(display));

        self.element_meta.insert(
            id,
            ElementMeta {
                description: format!(
                    "Pasted {} line{}",
                    line_count,
                    if line_count != 1 { "s" } else { "" },
                ),
            },
        );

        self.status = format!(
            "Pasted {} line{} (i to inline)",
            line_count,
            if line_count != 1 { "s" } else { "" },
        );
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> EventResult {
        // Don't forward mouse events during line-select mode.
        if self.line_select.is_some() {
            return EventResult::Unchanged;
        }

        // Middle-click: paste from system clipboard at the click position.
        if matches!(
            mouse.kind,
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle)
        ) {
            if let Ok(mut clip) = arboard::Clipboard::new()
                && let Ok(text) = clip.get_text()
            {
                // Place cursor at the click position first.
                self.textarea
                    .handle_mouse(mouse, self.textarea_area, self.textarea_state);
                self.handle_paste(&text);
                return EventResult::Redraw;
            }
            return EventResult::Unchanged;
        }

        // If the click lands on the "❯ " prompt char (2 cols left of textarea),
        // remap it to column 0 of the textarea so it places the cursor at the
        // start of that visual line.
        let mut mouse = mouse;
        let ta = self.textarea_area;
        if ta.width > 0
            && mouse.column >= ta.x.saturating_sub(2)
            && mouse.column < ta.x
            && mouse.row >= ta.y
            && mouse.row < ta.y + ta.height
        {
            mouse.column = ta.x;
        }

        let action = self
            .textarea
            .handle_mouse(mouse, self.textarea_area, self.textarea_state);

        // Check for element interactions (click, hover enter/leave).
        let mut had_element_event = false;
        if let Some(elem_event) = self.textarea.poll_element_event() {
            had_element_event = true;
            match elem_event.kind {
                TextElementEventKind::Click => {
                    if let Some(meta) = self.element_meta.get(&elem_event.id) {
                        self.status =
                            format!("Clicked element: {} (: to select lines)", meta.description);
                    } else {
                        self.status = format!("Clicked element {:?}", elem_event.id);
                    }
                }
                TextElementEventKind::HoverEnter => {
                    if let Some(meta) = self.element_meta.get(&elem_event.id) {
                        self.status = format!("Hovering: {}", meta.description);
                    }
                }
                TextElementEventKind::HoverLeave => {
                    if let Some(meta) = self.element_meta.get(&elem_event.id) {
                        self.status = format!("Left element: {}", meta.description);
                    } else {
                        self.status = format!("Left element {:?}", elem_event.id);
                    }
                }
            }
        }

        match action {
            MouseAction::CursorPlaced if !had_element_event => {
                let pos = self.textarea.cursor();
                self.status = format!("Click → cursor at byte {pos}");
                self.recompute_file_search();
                EventResult::Redraw
            }
            MouseAction::CursorPlaced => {
                // Element click already set the status — don't overwrite.
                self.recompute_file_search();
                EventResult::Redraw
            }
            MouseAction::SelectionUpdated => {
                if let Some(text) = self.textarea.selected_text() {
                    let chars = text.chars().count();
                    self.status = format!("Selecting… ({chars} chars)");
                }
                EventResult::Redraw
            }
            MouseAction::SelectionFinished => {
                if let Some(text) = self.textarea.take_clipboard() {
                    let chars = text.chars().count();
                    self.status = format!("Selected {chars} chars (copied to clipboard)");
                }
                EventResult::Redraw
            }
            MouseAction::Nothing if had_element_event => EventResult::Redraw,
            MouseAction::Nothing => EventResult::Unchanged,
            MouseAction::Scrolled => EventResult::Redraw,
        }
    }

    /// Confirm the currently-selected file search result.
    ///
    /// Replaces the `@query` text with an atomic element and inserts a trailing space.
    fn confirm_file_search(&mut self) {
        let Some(ctx) = compute_file_search_context(
            self.textarea.text(),
            self.textarea.cursor(),
            self.textarea.elements(),
        ) else {
            return;
        };
        let Some(path) = self.file_search.selected_path() else {
            return;
        };
        let path = path.to_owned();

        let element_text = build_file_ref_text(&path, None);
        let display = build_file_ref_display(&path, None);

        // Group: replace + trailing space = 1 undo step.
        self.textarea.begin_undo_group();

        let id = self.textarea.replace_range_with_element(
            ctx.range,
            &element_text,
            KIND_FILE_REF,
            Some(display),
        );

        // Insert trailing space so the user can keep typing.
        self.textarea.insert_str(" ");

        self.textarea.end_undo_group();

        self.element_meta.insert(
            id,
            ElementMeta {
                description: format!("File: {path}"),
            },
        );

        self.fs_active = false;
        self.file_search.clear();
        self.status = format!("Confirmed: @{path}");
    }

    /// Recompute file search context from current textarea state.
    fn recompute_file_search(&mut self) {
        let ctx = compute_file_search_context(
            self.textarea.text(),
            self.textarea.cursor(),
            self.textarea.elements(),
        );
        match ctx {
            Some(ctx) => {
                self.file_search.update(&ctx.query);
                self.fs_active = true;
                if self.file_search.is_visible() {
                    self.status = format!(
                        "@-search: \"{}\" ({} match{})",
                        ctx.query,
                        self.file_search.results.len(),
                        if self.file_search.results.len() != 1 {
                            "es"
                        } else {
                            ""
                        },
                    );
                }
            }
            None => {
                self.fs_active = false;
                self.file_search.clear();
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        // ── Line select mode takes ALL keys ──
        if self.line_select.is_some() {
            return self.handle_line_select_key(key);
        }

        // ── Global quit / clear ──
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.fs_active {
                    self.fs_active = false;
                    self.file_search.clear();
                    self.status = "File search dismissed.".into();
                    return EventResult::Redraw;
                }
                return EventResult::Quit;
            }
            KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return EventResult::Quit,
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self.textarea.is_empty() {
                    return EventResult::Quit;
                }
                self.textarea.set_text("");
                self.element_meta.clear();
                self.fs_active = false;
                self.file_search.clear();
                self.status = "Cleared.".into();
                return EventResult::Redraw;
            }
            _ => {}
        }

        // ── File search key interception (when dropdown is visible) ──
        if self.fs_active && self.file_search.is_visible() {
            match key {
                // ':' during file search → confirm file + open line select
                KeyEvent {
                    code: KeyCode::Char(':'),
                    modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                    ..
                } => {
                    self.enter_line_select_from_search();
                    return EventResult::Redraw;
                }
                KeyEvent {
                    code: KeyCode::Tab, ..
                }
                | KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    self.confirm_file_search();
                    return EventResult::Redraw;
                }
                KeyEvent {
                    code: KeyCode::Up, ..
                }
                | KeyEvent {
                    code: KeyCode::Char('p'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    self.file_search.move_selection(-1);
                    return EventResult::Redraw;
                }
                KeyEvent {
                    code: KeyCode::Down,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Char('n'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    self.file_search.move_selection(1);
                    return EventResult::Redraw;
                }
                _ => {} // fall through to textarea
            }
        }

        // ── ':' / Tab / Enter when cursor is on a file-ref element → open line select ──
        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char(':'),
                modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                ..
            } | KeyEvent {
                code: KeyCode::Tab | KeyCode::Enter,
                ..
            }
        ) && let Some(elem) = self.textarea.element_at_cursor()
            && elem.kind == KIND_FILE_REF
        {
            self.enter_line_select_from_element();
            return EventResult::Redraw;
        }

        // ── 'i' on any element → inline it; Tab/Enter on paste element → inline ──
        if let Some(elem) = self.textarea.element_at_cursor() {
            let is_i = matches!(
                key,
                KeyEvent {
                    code: KeyCode::Char('i'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
            );
            let is_tab_enter = matches!(
                key,
                KeyEvent {
                    code: KeyCode::Tab | KeyCode::Enter,
                    ..
                }
            );
            if is_i || (is_tab_enter && elem.kind == KIND_PASTE) {
                let id = elem.id;
                let desc = self
                    .element_meta
                    .remove(&id)
                    .map(|m| m.description)
                    .unwrap_or_else(|| "element".into());
                self.textarea.inline_element(id);
                self.status = format!("Inlined {desc}");
                self.recompute_file_search();
                return EventResult::Redraw;
            }
        }

        // ── Pass key to textarea, then recompute file search ──

        // Undo / Redo — intercept before passing to textarea.input().
        match key {
            KeyEvent {
                code: KeyCode::Char('z'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self.textarea.undo() {
                    self.status = "Undo.".into();
                } else {
                    self.status = "Nothing to undo.".into();
                }
                self.recompute_file_search();
                return EventResult::Redraw;
            }
            KeyEvent {
                code: KeyCode::Char('z'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::SHIFT) =>
            {
                if self.textarea.redo() {
                    self.status = "Redo.".into();
                } else {
                    self.status = "Nothing to redo.".into();
                }
                self.recompute_file_search();
                return EventResult::Redraw;
            }
            KeyEvent {
                code: KeyCode::Char('Z'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self.textarea.redo() {
                    self.status = "Redo.".into();
                } else {
                    self.status = "Nothing to redo.".into();
                }
                self.recompute_file_search();
                return EventResult::Redraw;
            }
            KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self.textarea.redo() {
                    self.status = "Redo.".into();
                } else {
                    self.status = "Nothing to redo.".into();
                }
                self.recompute_file_search();
                return EventResult::Redraw;
            }
            _ => {}
        }

        self.textarea.input(key);
        self.recompute_file_search();

        // Update status when file search is not active.
        if !self.fs_active {
            if let Some(elem) = self.textarea.element_at_cursor() {
                let id = elem.id;
                if let Some(meta) = self.element_meta.get(&id) {
                    self.status = format!(
                        "Cursor on element: {} (: to select lines)",
                        meta.description
                    );
                }
            } else {
                let elems = self.textarea.elements().len();
                let chars = self.textarea.text().len();
                self.status = format!(
                    "cursor: {} | {} chars | {} element{}",
                    self.textarea.cursor(),
                    chars,
                    elems,
                    if elems != 1 { "s" } else { "" },
                );
            }
        }

        EventResult::Redraw
    }

    // ── Line select entry / handling / confirm ──

    fn enter_line_select_from_search(&mut self) {
        // First, confirm the file search (create element).
        let Some(ctx) = compute_file_search_context(
            self.textarea.text(),
            self.textarea.cursor(),
            self.textarea.elements(),
        ) else {
            return;
        };
        let Some(path) = self.file_search.selected_path() else {
            return;
        };
        let path = path.to_owned();

        let element_text = build_file_ref_text(&path, None);
        let display = build_file_ref_display(&path, None);

        // Begin undo group — stays open until confirm/cancel line select.
        self.textarea.begin_undo_group();

        let id = self.textarea.replace_range_with_element(
            ctx.range,
            &element_text,
            KIND_FILE_REF,
            Some(display),
        );
        self.textarea.insert_str(" ");

        self.element_meta.insert(
            id,
            ElementMeta {
                description: format!("File: {path}"),
            },
        );
        self.fs_active = false;
        self.file_search.clear();

        // Now open line select.
        if let Some(mode) = LineSelectMode::open(path.clone(), id) {
            self.status =
                format!("{path} | j/k ↕ | C-u/C-d ½pg | f/b pg | v sel | Enter ok | Esc cancel");
            self.line_select = Some(mode);
        } else {
            // File unreadable — close the group immediately.
            self.textarea.end_undo_group();
            self.status = format!("Could not read: {path}");
        }
    }

    fn enter_line_select_from_element(&mut self) {
        let Some(elem) = self.textarea.element_at_cursor() else {
            return;
        };
        if elem.kind != KIND_FILE_REF {
            return;
        }
        let id = elem.id;
        let elem_text = self.textarea.element_text(id).unwrap_or("").to_string();
        let (path, existing_range) = parse_file_ref(&elem_text);
        let path = path.to_string();

        let Some(mut mode) = LineSelectMode::open(path.clone(), id) else {
            self.status = format!("Could not read: {path}");
            return;
        };

        // Begin undo group — stays open until confirm/cancel.
        self.textarea.begin_undo_group();

        // If there's an existing line range, scroll to it and show as locked.
        if let Some(range) = existing_range {
            let mid = (range.start() + range.end()) / 2;
            mode.goto_line(mid);
            mode.selection = SelectionState::Locked(
                range.start().saturating_sub(1),
                range.end().saturating_sub(1),
            );
        }

        self.status =
            format!("{path} | j/k ↕ | C-u/C-d ½pg | f/b pg | v sel | Enter ok | Esc cancel");
        self.line_select = Some(mode);
    }

    fn handle_line_select_key(&mut self, key: KeyEvent) -> EventResult {
        // Post-action to execute after the borrow of self.line_select is released.
        enum Action {
            Noop,
            Cancel,
            Confirm,
            LiveUpdate(Option<RangeInclusive<usize>>),
        }

        let action = {
            let Some(mode) = self.line_select.as_mut() else {
                return EventResult::Redraw;
            };

            match key {
                // Cancel: Esc, q, Ctrl-C
                KeyEvent {
                    code: KeyCode::Esc, ..
                }
                | KeyEvent {
                    code: KeyCode::Char('q'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => Action::Cancel,

                // Confirm: Enter
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => Action::Confirm,

                // v / V (Shift-V): toggle selection
                KeyEvent {
                    code: KeyCode::Char('v' | 'V'),
                    ..
                } => {
                    mode.toggle_selection();
                    // Only auto-clear on lock (second v), not while still selecting.
                    if matches!(mode.selection, SelectionState::Locked(..)) {
                        mode.check_select_all();
                    }
                    Action::LiveUpdate(mode.effective_range())
                }

                // g: first line
                KeyEvent {
                    code: KeyCode::Char('g'),
                    modifiers: KeyModifiers::NONE,
                    ..
                } => {
                    mode.goto_buf.clear();
                    mode.goto_line(1);
                    Action::Noop
                }
                // G: last line
                KeyEvent {
                    code: KeyCode::Char('G'),
                    ..
                } => {
                    mode.goto_buf.clear();
                    mode.goto_line(mode.total_lines());
                    Action::Noop
                }

                // j / Down: down 1
                KeyEvent {
                    code: KeyCode::Char('j'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    mode.goto_buf.clear();
                    mode.move_cursor(1);
                    Action::Noop
                }
                // k / Up: up 1
                KeyEvent {
                    code: KeyCode::Char('k'),
                    modifiers: KeyModifiers::NONE,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    mode.goto_buf.clear();
                    mode.move_cursor(-1);
                    Action::Noop
                }
                // Ctrl-D: half page down
                KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    mode.goto_buf.clear();
                    let half = (mode.viewport_height / 2).max(1) as isize;
                    mode.move_cursor(half);
                    Action::Noop
                }
                // Ctrl-U: half page up
                KeyEvent {
                    code: KeyCode::Char('u'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {
                    mode.goto_buf.clear();
                    let half = (mode.viewport_height / 2).max(1) as isize;
                    mode.move_cursor(-half);
                    Action::Noop
                }
                // f: full page down
                KeyEvent {
                    code: KeyCode::Char('f'),
                    modifiers: KeyModifiers::NONE,
                    ..
                } => {
                    mode.goto_buf.clear();
                    let page = mode.viewport_height.max(1) as isize;
                    mode.move_cursor(page);
                    Action::Noop
                }
                // b: full page up
                KeyEvent {
                    code: KeyCode::Char('b'),
                    modifiers: KeyModifiers::NONE,
                    ..
                } => {
                    mode.goto_buf.clear();
                    let page = mode.viewport_height.max(1) as isize;
                    mode.move_cursor(-page);
                    Action::Noop
                }
                // Digits: accumulate goto-line input and jump immediately
                KeyEvent {
                    code: KeyCode::Char(c),
                    modifiers: KeyModifiers::NONE,
                    ..
                } if c.is_ascii_digit() => {
                    mode.goto_buf.push(c);
                    if let Ok(line) = mode.goto_buf.parse::<usize>() {
                        mode.goto_line(line);
                    }
                    Action::Noop
                }
                // Any other key clears goto buffer
                _ => {
                    mode.goto_buf.clear();
                    Action::Noop
                }
            }
        };

        // Execute action (line_select borrow is released).
        match action {
            Action::Cancel => {
                self.cancel_line_select();
                return EventResult::Redraw;
            }
            Action::Confirm => {
                self.confirm_line_select();
                return EventResult::Redraw;
            }
            Action::LiveUpdate(range) => {
                self.update_line_select_element(range.as_ref());
            }
            Action::Noop => {
                // Live-update element while actively selecting (range follows cursor).
                let selecting_range = self.line_select.as_ref().and_then(|m| {
                    if matches!(m.selection, SelectionState::Selecting(_)) {
                        m.effective_range()
                    } else {
                        None
                    }
                });
                if let Some(range) = selecting_range {
                    self.update_line_select_element(Some(&range));
                }
            }
        }

        // Update status.
        self.update_line_select_status();
        EventResult::Redraw
    }

    fn cancel_line_select(&mut self) {
        let Some(_mode) = self.line_select.take() else {
            return;
        };

        // Cancel the undo group — restores textarea to pre-line-select state.
        // No manual element revert needed.
        self.textarea.cancel_undo_group();

        self.status = "Line select cancelled.".into();
    }

    fn confirm_line_select(&mut self) {
        let Some(mode) = self.line_select.take() else {
            return;
        };

        let mut range = mode.effective_range();
        // Selecting all lines = entire file = no range needed.
        if let Some(ref r) = range
            && *r.start() == 1
            && *r.end() == mode.total_lines()
        {
            range = None;
        }

        let new_text = build_file_ref_text(&mode.file_path, range.as_ref());
        let new_display = build_file_ref_display(&mode.file_path, range.as_ref());

        // Find the element's current range in the buffer.
        let elem_range = self
            .textarea
            .elements()
            .iter()
            .find(|e| e.id == mode.element_id)
            .map(|e| e.range.clone());

        if let Some(elem_range) = elem_range {
            let new_id = self.textarea.replace_range_with_element(
                elem_range,
                &new_text,
                KIND_FILE_REF,
                Some(new_display),
            );

            let desc = match &range {
                Some(r) if r.start() == r.end() => {
                    format!("File: {}:{}", mode.file_path, r.start())
                }
                Some(r) => format!("File: {}:{}-{}", mode.file_path, r.start(), r.end()),
                None => format!("File: {}", mode.file_path),
            };
            self.element_meta.insert(
                new_id,
                ElementMeta {
                    description: desc.clone(),
                },
            );
            self.status = format!("Confirmed: {desc}");
        }

        // Close the undo group — all line-select mutations become 1 undo step.
        self.textarea.end_undo_group();
    }

    /// Live-update the element text/display to reflect a line range change.
    fn update_line_select_element(&mut self, range: Option<&RangeInclusive<usize>>) {
        // Extract data before mutating.
        let (file_path, old_element_id) = {
            let mode = self.line_select.as_ref().unwrap();
            (mode.file_path.clone(), mode.element_id)
        };

        let elem_range = self
            .textarea
            .elements()
            .iter()
            .find(|e| e.id == old_element_id)
            .map(|e| e.range.clone());
        let Some(elem_range) = elem_range else {
            return;
        };

        let new_text = build_file_ref_text(&file_path, range);
        let new_display = build_file_ref_display(&file_path, range);
        let new_id = self.textarea.replace_range_with_element(
            elem_range,
            &new_text,
            KIND_FILE_REF,
            Some(new_display),
        );

        let desc = match range {
            Some(r) if r.start() == r.end() => format!("File: {}:{}", file_path, r.start()),
            Some(r) => format!("File: {}:{}-{}", file_path, r.start(), r.end()),
            None => format!("File: {}", file_path),
        };
        self.element_meta
            .insert(new_id, ElementMeta { description: desc });

        // Update element_id in line_select so subsequent ops find the right element.
        if let Some(mode) = self.line_select.as_mut() {
            mode.element_id = new_id;
        }
    }

    fn update_line_select_status(&mut self) {
        if let Some(mode) = &self.line_select {
            let line_info = format!("L{}/{}", mode.cursor_line + 1, mode.total_lines());
            let sel_info = match mode.selection {
                SelectionState::None => String::new(),
                SelectionState::Selecting(anchor) => {
                    let (s, e) = sorted(anchor, mode.cursor_line);
                    format!(" | selecting {}‑{}", s + 1, e + 1)
                }
                SelectionState::Locked(s, e) => format!(" | locked {}‑{}", s + 1, e + 1),
            };
            let goto_info = if mode.goto_buf.is_empty() {
                String::new()
            } else {
                format!(" | :{}", mode.goto_buf)
            };
            self.status = format!("{} {line_info}{sel_info}{goto_info}", mode.file_path);
        }
    }

    // ── Rendering ──

    fn render(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
        terminal.draw(|f| {
            let area = f.area();

            if self.line_select.is_some() {
                // ── Line select layout: preview + hints + prompt + status ──
                let [preview_area, hints_area, prompt_outer, status_area] = Layout::vertical([
                    Constraint::Min(5),
                    Constraint::Length(1),
                    Constraint::Length(5),
                    Constraint::Length(1),
                ])
                .areas(area);

                // Update viewport height in the mode so scrolling works correctly.
                if let Some(mode) = self.line_select.as_mut() {
                    // Reserve 2 rows for border.
                    mode.viewport_height = preview_area.height.saturating_sub(2) as usize;
                }

                self.render_line_select(f.buffer_mut(), preview_area);
                self.render_line_select_hints(f.buffer_mut(), hints_area);
                self.render_prompt(f, prompt_outer);

                let status = Paragraph::new(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(&self.status, Style::default().fg(Color::DarkGray)),
                ]));
                status.render(status_area, f.buffer_mut());
            } else {
                // ── Normal layout ──
                let info_rows: u16 = 16;
                let fs_rows = if self.fs_active {
                    self.file_search.dropdown_height()
                } else {
                    0
                };
                // Ensure the prompt gets at least 5 rows (3 inner + border).
                let min_prompt: u16 = 5;
                let fixed = info_rows + 1 + fs_rows + 1;
                let remaining = area.height.saturating_sub(fixed);
                let half = (remaining / 2).max(min_prompt);

                let [
                    info_area,
                    _gap,
                    raw_buf_area,
                    fs_area,
                    prompt_outer,
                    status_area,
                ] = Layout::vertical([
                    Constraint::Length(info_rows),
                    Constraint::Length(1),
                    Constraint::Length(half),
                    Constraint::Length(fs_rows),
                    Constraint::Length(half),
                    Constraint::Length(1),
                ])
                .areas(area);

                self.render_info(f.buffer_mut(), info_area);
                self.render_raw_buffer(f.buffer_mut(), raw_buf_area);

                if fs_rows > 0 {
                    self.render_file_search(f.buffer_mut(), fs_area);
                }

                self.render_prompt(f, prompt_outer);

                let status = Paragraph::new(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(&self.status, Style::default().fg(Color::DarkGray)),
                ]));
                status.render(status_area, f.buffer_mut());
            }

            // ── Cursor management (inside draw) ──
            //
            // By calling set_cursor_position inside the draw closure,
            // ratatui emits show_cursor + set_cursor_position WITHOUT
            // the hide_cursor that happens when no cursor is set.  This
            // avoids the hide→show cycle that resets the terminal's
            // blink timer every frame.
            let want_cursor = if self.line_select.is_none() {
                self.textarea
                    .cursor_pos_with_state(self.textarea_area, self.textarea_state)
            } else {
                None
            };

            if let Some((cx, cy)) = want_cursor {
                f.set_cursor_position(ratatui::layout::Position { x: cx, y: cy });
            }
        })?;

        Ok(())
    }

    /// Render the prompt box (shared between normal and line-select layouts).
    fn render_prompt(&mut self, f: &mut ratatui::Frame<'_>, prompt_outer: Rect) {
        let prompt_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Prompt ",
                Style::default().fg(Color::White).bold(),
            ));
        let prompt_inner = prompt_block.inner(prompt_outer);
        prompt_block.render(prompt_outer, f.buffer_mut());

        if prompt_inner.width > 2 {
            let [char_area, textarea_area] =
                Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).areas(prompt_inner);

            let prompt_char = Span::styled("❯ ", Style::default().fg(Color::Magenta).bold());
            f.buffer_mut().set_string(
                char_area.x,
                char_area.y,
                &prompt_char.content,
                prompt_char.style,
            );

            StatefulWidgetRef::render_ref(
                &(&self.textarea),
                textarea_area,
                f.buffer_mut(),
                &mut self.textarea_state,
            );

            // Store the textarea render area for mouse mapping.
            self.textarea_area = textarea_area;
        }
    }

    /// Render the file preview with line numbers and selection highlighting.
    fn render_line_select(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let Some(mode) = &self.line_select else {
            return;
        };

        // Title shows file path + range if any.
        let title = match mode.effective_range() {
            Some(r) if r.start() == r.end() => format!(" {} :{} ", mode.file_path, r.start()),
            Some(r) => format!(" {} :{}-{} ", mode.file_path, r.start(), r.end()),
            None => format!(" {} ", mode.file_path),
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(80, 80, 80)))
            .title(Span::styled(
                title,
                Style::default().fg(Color::White).bold(),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width < 6 || inner.height == 0 {
            return;
        }

        let total = mode.total_lines();
        let gutter_width = total.to_string().len();
        let code_start_col = inner.x + gutter_width as u16 + 1; // +1 for separator
        let code_width = inner.width.saturating_sub(gutter_width as u16 + 1);

        // Styles.
        let gutter_style = Style::default().fg(Color::Rgb(80, 80, 80));
        let gutter_cursor_style = Style::default().fg(Color::Yellow);
        let code_style = Style::default().fg(Color::Rgb(200, 200, 200));
        let cursor_bg = Style::default().fg(Color::White).bg(Color::Rgb(50, 50, 60));
        let selecting_bg = Style::default().fg(Color::White).bg(Color::Rgb(30, 60, 30));
        let locked_bg = Style::default().fg(Color::White).bg(Color::Rgb(70, 35, 35));
        let sep = "│";

        for row in 0..inner.height as usize {
            let line_idx = mode.scroll_top + row;
            if line_idx >= total {
                break;
            }
            let y = inner.y + row as u16;
            let line_num = line_idx + 1; // 1-indexed for display

            // Determine line style.
            let is_cursor = line_idx == mode.cursor_line;
            let is_sel = mode.is_selected(line_idx);

            let line_style = if is_cursor {
                cursor_bg
            } else if is_sel {
                match mode.selection {
                    SelectionState::Selecting(_) => selecting_bg,
                    SelectionState::Locked(_, _) => locked_bg,
                    SelectionState::None => code_style,
                }
            } else {
                code_style
            };

            // Line number gutter.
            let num_str = format!("{:>width$}", line_num, width = gutter_width);
            let g_style = if is_cursor {
                gutter_cursor_style
            } else {
                gutter_style
            };
            buf.set_string(inner.x, y, &num_str, g_style);
            buf.set_string(inner.x + gutter_width as u16, y, sep, gutter_style);

            // Code content.
            let content = &mode.lines[line_idx];
            // Fill the entire code area with background first if highlighted.
            if is_cursor || is_sel {
                for col in 0..code_width {
                    buf.set_string(code_start_col + col, y, " ", line_style);
                }
            }
            // Render the actual text (truncate to fit).
            let display: String = content.chars().take(code_width as usize).collect();
            buf.set_string(code_start_col, y, &display, line_style);
        }

        // Render the goto-line input at the bottom-right of the preview if active.
        if !mode.goto_buf.is_empty() {
            let goto_str = format!(":{}", mode.goto_buf);
            let w = goto_str.len() as u16;
            let x = area.x + area.width.saturating_sub(w + 2);
            let y = area.y + area.height.saturating_sub(1);
            buf.set_string(x, y, &goto_str, Style::default().fg(Color::Yellow).bold());
        }
    }

    fn render_line_select_hints(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let k = Style::default().fg(Color::Yellow);
        let d = Style::default().fg(Color::DarkGray);
        let s = Style::default().fg(Color::Rgb(50, 50, 50));

        let hints = Line::from(vec![
            Span::styled(" j/k", k),
            Span::styled(" ↕  ", d),
            Span::styled("│", s),
            Span::styled(" C-u/d", k),
            Span::styled(" ½pg  ", d),
            Span::styled("│", s),
            Span::styled(" f/b", k),
            Span::styled(" pg  ", d),
            Span::styled("│", s),
            Span::styled(" v/V", k),
            Span::styled(" select  ", d),
            Span::styled("│", s),
            Span::styled(" 0‑9", k),
            Span::styled(" goto  ", d),
            Span::styled("│", s),
            Span::styled(" g/G", k),
            Span::styled(" top/bot  ", d),
            Span::styled("│", s),
            Span::styled(" Enter", k),
            Span::styled(" confirm  ", d),
            Span::styled("│", s),
            Span::styled(" Esc/q", k),
            Span::styled(" cancel", d),
        ]);

        Paragraph::new(hints).render(area, buf);
    }

    fn render_file_search(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .title(Span::styled(
                " @ File Search ",
                Style::default().fg(Color::Green).bold(),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let match_style = Style::default().fg(Color::Yellow).bold();
        let selected_style = Style::default().fg(Color::White);
        let normal_style = Style::default().fg(Color::Gray);
        let marker_style = Style::default().fg(Color::Green).bold();
        let dim_style = Style::default().fg(Color::DarkGray);

        for (i, result) in self
            .file_search
            .results
            .iter()
            .enumerate()
            .take(inner.height as usize)
        {
            let is_selected = i == self.file_search.selected;
            let y = inner.y + i as u16;

            // Selection marker
            let marker = if is_selected { "▸ " } else { "  " };
            buf.set_string(inner.x, y, marker, marker_style);

            // Path with fuzzy-match highlighting
            let base_style = if is_selected {
                selected_style
            } else {
                normal_style
            };
            let mut col = inner.x + 2;
            for (ci, ch) in result.path.chars().enumerate() {
                if col >= inner.x + inner.width {
                    break;
                }
                let style = if result.indices.contains(&ci) {
                    match_style
                } else {
                    base_style
                };
                let s = ch.to_string();
                buf.set_string(col, y, &s, style);
                col += unicode_width::UnicodeWidthStr::width(s.as_str()) as u16;
            }

            // Show score on the right for selected item
            if is_selected && result.score > 0 {
                let score_str = format!(" [{}]", result.score);
                let score_w = score_str.len() as u16;
                if inner.width > score_w + 4 {
                    let sx = inner.x + inner.width - score_w;
                    buf.set_string(sx, y, &score_str, dim_style);
                }
            }
        }
    }

    fn render_raw_buffer(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        // Build the block title: "Raw Buffer" + clipboard summary.
        let mut title_spans = vec![Span::styled(
            " Raw Buffer ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        )];
        if let Some(clip) = self.textarea.clipboard() {
            let preview: String = clip.chars().take(40).collect();
            let suffix = if clip.chars().count() > 40 { "…" } else { "" };
            title_spans.push(Span::styled(
                format!("│ clipboard: {preview}{suffix} "),
                Style::default().fg(Color::Rgb(80, 140, 80)),
            ));
        }

        let raw_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
            .title(Line::from(title_spans));
        let inner = raw_block.inner(area);
        raw_block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let text = self.textarea.text();
        let elements = self.textarea.elements();
        let plain_style = Style::default().fg(Color::Rgb(160, 160, 160));
        let plain_nl = Style::default().fg(Color::Rgb(80, 80, 80));
        let elem_style = Style::default().fg(Color::Rgb(200, 140, 60));
        let elem_nl = Style::default().fg(Color::Rgb(120, 80, 30));

        let mut display_spans: Vec<Span<'_>> = Vec::new();
        let mut pos = 0;

        for elem in elements {
            if pos < elem.range.start {
                push_with_visible_newlines(
                    &text[pos..elem.range.start],
                    plain_style,
                    plain_nl,
                    &mut display_spans,
                );
            }
            push_with_visible_newlines(
                &text[elem.range.clone()],
                elem_style,
                elem_nl,
                &mut display_spans,
            );
            pos = elem.range.end;
        }

        if pos < text.len() {
            push_with_visible_newlines(&text[pos..], plain_style, plain_nl, &mut display_spans);
        }

        let line = Line::from(display_spans);
        let opts = RtOptions::new(inner.width as usize).break_words(true);
        let wrapped = word_wrap_line(&line, opts);

        let para = Paragraph::new(Text::from(wrapped));
        para.render(inner, buf);
    }

    fn render_info(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("TextArea Demo", Style::default().fg(Color::White).bold()),
                Span::styled(
                    " — @-File-Search + Atomic Elements",
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(""),
        ];

        let bindings = vec![
            ("@query", "trigger file search"),
            ("Tab / Enter", "confirm file selection"),
            ("↑/↓ / C-p/C-n", "navigate results"),
            ("Esc", "dismiss search / quit"),
            ("Paste (Cmd+V)", "create paste element"),
            ("i", "inline element at cursor"),
            ("←/→", "navigate (jumps over elements)"),
            ("Backspace/Del", "delete (atomic for elements)"),
            ("Alt+←/→", "word navigation"),
            ("Ctrl+A/E", "beginning/end of line"),
            ("Ctrl+K/U", "kill to end/beginning of line"),
            ("Ctrl+Z", "undo"),
            ("Ctrl+Shift+Z/Y", "redo"),
            ("Ctrl+C", "clear (quit if empty)"),
        ];

        for (key, desc) in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>16}", key), Style::default().fg(Color::Yellow)),
                Span::styled("  ", Style::default()),
                Span::styled(desc, Style::default().fg(Color::Gray)),
            ]));
        }

        let text = Text::from(lines);
        let para = Paragraph::new(text);
        para.render(area, buf);
    }
}

/// Split `s` at newlines, push text segments with `text_style` and
/// literal `\n` markers with `nl_style` (dim).
fn push_with_visible_newlines<'a>(
    s: &'a str,
    text_style: Style,
    nl_style: Style,
    out: &mut Vec<Span<'a>>,
) {
    let mut first = true;
    for part in s.split('\n') {
        if !first {
            out.push(Span::styled("\\n", nl_style));
        }
        if !part.is_empty() {
            out.push(Span::styled(part, text_style));
        }
        first = false;
    }
}

fn main() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    stdout().execute(EnableMouseCapture)?;
    stdout().execute(EnableBlinking)?;
    stdout().execute(SetCursorStyle::BlinkingBlock)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal);

    let _ = stdout().execute(DisableMouseCapture);
    let _ = stdout().execute(DisableBracketedPaste);
    let _ = stdout().execute(SetCursorStyle::DefaultUserShape);
    let _ = terminal::disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);

    if let Err(ref e) = result {
        eprintln!("textarea_demo exited with error: {e}");
    }

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = DemoApp::new();
    app.render(terminal)?;

    loop {
        // The textarea tells us when it needs a timer tick (e.g. for
        // continuous drag-scrolling).  Use its timeout for poll, falling
        // back to a generous default that lets the cursor blink.
        let timeout = app
            .textarea
            .poll_timeout_ms()
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(100));

        if crossterm::event::poll(timeout)? {
            let event = crossterm::event::read()?;
            match app.handle_event(event) {
                EventResult::Quit => break,
                EventResult::Redraw => app.render(terminal)?,
                EventResult::Unchanged => {}
            }
        }

        // Whether we processed an event or timed out, check if the
        // textarea has pending timer work (e.g. continuous drag-scroll).
        // The textarea's internal throttle prevents this from firing
        // too fast — it'll return Nothing if not enough time has passed.
        if app.textarea.poll_timeout_ms().is_some() {
            let action = app.textarea.tick(app.textarea_area, app.textarea_state);
            if matches!(action, MouseAction::SelectionUpdated) {
                if let Some(text) = app.textarea.selected_text() {
                    let chars = text.chars().count();
                    app.status = format!("Selecting… ({chars} chars)");
                }
                app.render(terminal)?;
            }
        }
    }

    Ok(())
}
