//! Block viewer — fullscreen content viewer for scrollback blocks.
//!
//! Opens via Ctrl-F on a selected block, replaces the scrollback area.
//! Provides ListPane-based navigation, search, visual-select, and copy.
//!
//! Supports thinking/agent message blocks (markdown content).
//! Execute and edit viewers will be added in later phases.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use echo_agent_workspace::permission::mcp_titleize_segment;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidget;

use crate::clipboard::SystemClipboard;
use crate::render::scrollbar::SCROLLBAR_TOTAL_COLS;
use crate::scrollback::block::{BlockContent, RenderBlock};
use crate::scrollback::blocks::ToolCallBlock;
use crate::scrollback::entry::{EntryId, ScrollbackEntry};
use crate::scrollback::types::{BlockContext, DisplayMode};
use crate::theme::{Theme, ThemeKind};
use crate::views::list_pane::{
    ListItem, ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode,
};
use crate::views::modal_window::ModalWindowState;
use crate::views::shortcuts_bar::HintItem;

// ---------------------------------------------------------------------------
// ContentLine — generic ListItem for the viewer
// ---------------------------------------------------------------------------

/// A single line of content displayed in the block viewer's ListPane.
#[derive(Clone)]
pub struct ContentLine {
    /// Styled content to display.
    content: Line<'static>,
    /// Plain text for search matching.
    plain_text: String,
    /// Stable identity (pre-wrap line index).
    id: u64,
    /// Optional full-width background color (e.g., code block bg).
    bg: Option<Color>,
}

impl ContentLine {
    /// Build content lines from pre-wrap rendered markdown lines.
    pub fn from_lines(lines: &[Line<'static>]) -> Vec<Self> {
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                Self {
                    content: line.clone(),
                    plain_text: plain,
                    id: i as u64,
                    bg: line.style.bg,
                }
            })
            .collect()
    }
}

impl ListItem for ContentLine {
    fn content(&self) -> &Line<'_> {
        &self.content
    }

    fn stable_id(&self) -> u64 {
        self.id
    }

    fn search_text(&self) -> &str {
        &self.plain_text
    }

    fn background(&self) -> Option<Color> {
        self.bg
    }
}

// ---------------------------------------------------------------------------
// DiffLineMeta — per-item diff metadata for edit viewer patch copy
// ---------------------------------------------------------------------------

/// Metadata for a single diff line, stored parallel to `items` in the edit viewer.
pub struct DiffLineMeta {
    pub tag: similar::ChangeTag,
    pub text: String,
    pub lo: usize,
    pub ln: usize,
}

// ---------------------------------------------------------------------------
// BlockViewerPane
// ---------------------------------------------------------------------------

/// What kind of block content the viewer is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerKind {
    /// Thinking or AgentMessage (markdown content).
    Markdown,
    /// Execute tool call (stdout).
    Execute,
    /// Edit tool call (diff).
    Edit,
    /// Background task (stdout from central store).
    BgTask,
    /// Web fetch tool call (fetched content).
    WebFetch,
    /// Web search tool call (search results + citations).
    WebSearch,
    /// Integration tool discovery (search_tool).
    IntegrationSearch,
    /// Integration tool dispatch (use_tool).
    UseTool,
    /// Read file tool call (file content).
    Read,
    Grep,
    /// Plain text content (e.g., catalog entry).
    PlainText,
}

/// Fullscreen block content viewer.
///
/// Replaces the scrollback area when open. Owns a `ListPaneState` for
/// navigation, search, and visual-select. Reads block content from the
/// scrollback via `entry_id`.
pub struct BlockViewerPane {
    /// Which scrollback entry we are viewing.
    pub entry_id: EntryId,
    /// What kind of content.
    pub kind: ViewerKind,
    /// ListPane state (scroll, selection, search, follow).
    pub list_state: ListPaneState,
    /// Visual style for the ListPane.
    list_style: ListPaneStyle,
    /// Content items for the ListPane.
    items: Vec<ContentLine>,
    /// Cached content area from last render (for mouse hit-testing).
    last_content_area: Rect,

    /// Set by handle_key when 'r' is pressed. Caller should toggle raw mode
    /// on the entry and call rebuild_items().
    pub raw_toggle_pending: bool,
    /// Set by handle_key when 'Y' is pressed. Caller should copy the command.
    pub copy_meta_pending: bool,
    /// Set by handle_key when 'y' is pressed on edit blocks. Caller should copy patch.
    pub copy_content_pending: bool,
    /// Per-item diff metadata for edit viewer (parallel to `items`).
    /// `None` entries are separator lines between hunks.
    diff_meta: Vec<Option<DiffLineMeta>>,
    /// Last observed content generation (for streaming change detection).
    last_generation: u64,
    /// Whether the block was running when the viewer last checked.
    /// Used to detect the running→finished transition (disable follow once).
    was_running: bool,
    /// Task ID for BgTask viewers (for looking up stdout in central store).
    pub bg_task_id: Option<String>,
    /// Last theme kind seen — used to detect theme switches and restyle.
    last_theme: ThemeKind,
    /// Modal chrome state (close button hover, popup area, etc.).
    pub modal: ModalWindowState,
    /// Cached prepend (preamble) ContentLines from the last render. Combined
    /// with `items` to produce the unified vec the ListPane sees. Needed so
    /// scroll / mouse / key handlers index into the same item space the
    /// layout was prepared against (`prepared_items_count`).
    prepend_items: Vec<ContentLine>,
    /// Text-level drag selection (character-precise, like the scrollback
    /// pane). Anchor and head are STABLE references to characters in the
    /// unified items list — they survive scrolling so copy includes
    /// off-screen content. Active between `Down(Left)` inside the content
    /// area and the matching `Up(Left)`. Stays visible after release until
    /// the next click.
    pub text_drag: Option<TextDrag>,
    /// Text copied on the last drag release. The `Up(Left)` handler
    /// extracts the text immediately (same as scrollback
    /// `finish_text_drag`); the caller drains this with `.take()` and
    /// copies it to the clipboard.
    pub drag_copy_text: Option<String>,
    /// Cached unified items vec (prepend + body). Rebuilt by
    /// `rebuild_unified_cache` to avoid re-cloning on every handler and
    /// render call within the same frame. Callers that hold `&mut self`
    /// call `rebuild_unified_cache()` first, then reference
    /// `self.cached_unified` via a disjoint field borrow.
    cached_unified: Vec<ContentLine>,
}

/// One end of a character-level text selection.
///
/// References a position in the unified items list (preamble + body) by
/// the stable `item_idx` and a display-column offset within that item's
/// logical line text. Stable across scroll: `item_idx` doesn't change
/// when the viewport moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextEndpoint {
    /// Index into the unified items vec (`prepend_items` then `items`).
    pub item_idx: usize,
    /// Display-column offset within `items[item_idx]`'s logical line text.
    /// Zero-based; `0` means "before the first character".
    pub col: u16,
}

/// Character-precise text selection within the modal content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextDrag {
    pub anchor: TextEndpoint,
    pub head: TextEndpoint,
    /// `true` while the mouse button is still down. Once released, the
    /// selection stays visible (for visual feedback) but isn't extended
    /// further.
    pub active: bool,
}

impl TextDrag {
    /// Endpoints in reading order (`start <= end`).
    pub fn ordered(&self) -> (TextEndpoint, TextEndpoint) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether the selection covers at least one character.
    pub fn is_non_empty(&self) -> bool {
        let (s, e) = self.ordered();
        s != e
    }
}

impl BlockViewerPane {
    /// Create a viewer for a markdown block (thinking or agent message).
    pub fn for_markdown(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let lines = Self::extract_markdown_lines(&entry.block)?;
        let generation = Self::extract_generation(&entry.block).unwrap_or(0);

        let config = ListPaneConfig {
            follow_enabled: entry.is_running,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state =
            ListPaneState::new_with_config(WrapMode::Wrap, entry.is_running, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let items = ContentLine::from_lines(&lines);

        Some(Self {
            entry_id,
            kind: ViewerKind::Markdown,
            list_state,
            list_style: ListPaneStyle {
                uniform_visual_bg: true,
                ..ListPaneStyle::default()
            },
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta: Vec::new(),
            last_generation: generation,
            was_running: entry.is_running,
            bg_task_id: None,
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        })
    }

    /// Create a viewer for an execute block (stdout output).
    pub fn for_execute(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) = &entry.block else {
            return None;
        };

        let config = ListPaneConfig {
            follow_enabled: entry.is_running,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state =
            ListPaneState::new_with_config(WrapMode::Wrap, entry.is_running, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let theme = Theme::current();
        let items = Self::build_execute_items(exec.output.as_deref(), &theme);
        let last_output_len = exec.output.as_ref().map_or(0, |o| o.len());

        // Dark background style for terminal output
        let list_style = ListPaneStyle {
            uniform_visual_bg: true,
            ..ListPaneStyle::default()
        };

        Some(Self {
            entry_id,
            kind: ViewerKind::Execute,
            list_state,
            list_style,
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta: Vec::new(),
            last_generation: last_output_len as u64, // reuse generation field for output length
            was_running: entry.is_running,
            bg_task_id: None,
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        })
    }

    /// Create a viewer for static content lines (shared by web_fetch and web_search).
    fn for_static_content(entry_id: EntryId, kind: ViewerKind, lines: Vec<Line<'static>>) -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::Wrap, false, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let items: Vec<ContentLine> = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                ContentLine {
                    content: line,
                    plain_text: plain,
                    id: i as u64,
                    bg: None,
                }
            })
            .collect();

        Self {
            entry_id,
            kind,
            list_state,
            list_style: ListPaneStyle::default(),
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta: Vec::new(),
            last_generation: 0,
            was_running: false,
            bg_task_id: None,
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        }
    }

    /// Create a viewer for a web fetch block (fetched content).
    pub fn for_web_fetch(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::WebFetch(fetch)) = &entry.block else {
            return None;
        };

        let theme = Theme::current();
        let content = fetch.output.as_deref().unwrap_or("");
        let lines: Vec<Line<'static>> = content
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.text_primary),
                ))
            })
            .collect();

        Some(Self::for_static_content(
            entry_id,
            ViewerKind::WebFetch,
            lines,
        ))
    }

    /// Create a viewer for a read file block (file content with syntax highlighting
    /// and absolute line numbers, or error text if content is absent).
    pub fn for_read(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::Read(read)) = &entry.block else {
            return None;
        };

        let theme = Theme::current();

        // Error-only blocks: show the error text
        if !read.has_content() {
            let error = read.error.as_deref()?;
            let error_style = Style::default().fg(theme.accent_error);
            let lines: Vec<Line<'static>> = error
                .lines()
                .map(|l| Line::from(Span::styled(l.to_string(), error_style)))
                .collect();
            return Some(Self::for_static_content(entry_id, ViewerKind::Read, lines));
        }

        let content = read.content.as_deref().unwrap_or("");
        let raw_lines: Vec<&str> = content.lines().collect();
        let base_line = read.line_range.map_or(1, |r| r.start);
        let max_line = base_line + raw_lines.len().saturating_sub(1);
        let gutter_width = max_line.checked_ilog10().map_or(1, |d| d as usize + 1);
        let gutter_style = Style::default().fg(theme.gray_dim);
        let fallback = Style::default().fg(theme.text_primary);

        let syntect = crate::syntax::get_syntect();
        let mut highlighter =
            syntect.highlight_lines_by_file_path(std::path::Path::new(&read.path));

        let lines: Vec<Line<'static>> = raw_lines
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let gutter = format!("{:>w$}  ", base_line + i, w = gutter_width);
                let mut spans = vec![Span::styled(gutter, gutter_style)];
                spans.extend(crate::syntax::highlight_line(
                    text,
                    &mut highlighter,
                    syntect,
                    fallback,
                ));
                Line::from(spans)
            })
            .collect();

        Some(Self::for_static_content(entry_id, ViewerKind::Read, lines))
    }

    fn static_lines_from_block(
        entry: &ScrollbackEntry,
        block: &impl BlockContent,
    ) -> Vec<Line<'static>> {
        let ctx = BlockContext {
            mode: DisplayMode::Expanded,
            is_running: entry.is_running,
            width: 120,
            raw: false,
            max_lines: None,
            appearance: Default::default(),
            is_selected: false,
            cwd: None,
        };
        block
            .output(&ctx)
            .lines
            .into_iter()
            .map(|bl| bl.content)
            .collect()
    }

    pub fn for_grep(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::Search(search)) = &entry.block else {
            return None;
        };

        let lines = Self::static_lines_from_block(entry, search);
        Some(Self::for_static_content(entry_id, ViewerKind::Grep, lines))
    }

    pub fn for_list_dir(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::ListDir(list_dir)) = &entry.block else {
            return None;
        };

        let lines = Self::static_lines_from_block(entry, list_dir);
        Some(Self::for_static_content(
            entry_id,
            ViewerKind::PlainText,
            lines,
        ))
    }

    /// Create a viewer for a web search block (search results + citations).
    pub fn for_web_search(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::WebSearch(ws)) = &entry.block else {
            return None;
        };

        let theme = Theme::current();
        let content = ws.content.as_deref().unwrap_or("");

        let mut lines: Vec<Line<'static>> = content
            .lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.text_primary),
                ))
            })
            .collect();

        // Citations footer (separated by a horizontal rule).
        if !ws.citations.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "───────────────────────────────────",
                Style::default().fg(theme.gray_dim),
            )));
            lines.push(Line::from(Span::styled(
                format!("Sources ({})", ws.citations.len()),
                Style::default().fg(theme.text_secondary),
            )));
            let url_style = Style::default().fg(theme.gray);
            let prefix_style = Style::default().fg(theme.text_secondary);
            for (i, url) in ws.citations.iter().enumerate() {
                let prefix = format!("[{}] ", i + 1);
                lines.push(Line::from(vec![
                    Span::styled(prefix, prefix_style),
                    Span::styled(url.clone(), url_style),
                ]));
            }
        }

        Some(Self::for_static_content(
            entry_id,
            ViewerKind::WebSearch,
            lines,
        ))
    }

    /// Create a viewer for a search_tool block.
    ///
    /// Preamble renders the styled header ("Search Tools query").
    /// Body shows limit, result count, then the structured tool list.
    pub fn for_integration_search(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::IntegrationSearch(st)) = &entry.block else {
            return None;
        };

        let theme = Theme::current();
        let label = Style::default().fg(theme.text_secondary);
        let value = Style::default().fg(theme.text_primary);
        let dim = Style::default().fg(theme.gray);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Metadata
        if let Some(limit) = st.limit {
            lines.push(Line::from(vec![
                Span::styled("limit: ", label),
                Span::styled(limit.to_string(), value),
            ]));
        }
        let s = if st.result_count == 1 { "" } else { "s" };
        lines.push(Line::from(Span::styled(
            format!("{} result{s}", st.result_count),
            label,
        )));

        // Tool list
        for (i, tool) in st.results.iter().enumerate() {
            lines.push(Line::from(""));
            let action =
                mcp_titleize_segment(crate::scrollback::blocks::discovered_tool_action(tool));
            let server = mcp_titleize_segment(&tool.server);
            lines.push(Line::from(vec![
                Span::styled(format!("{}. ", i + 1), dim),
                Span::styled(action, Style::default().fg(theme.text_primary)),
                Span::styled(format!("  {}", server), dim),
            ]));
            if !tool.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("   {}", tool.description),
                    dim,
                )));
            }
        }

        Some(Self::for_static_content(
            entry_id,
            ViewerKind::IntegrationSearch,
            lines,
        ))
    }

    /// Create a viewer for a use_tool block.
    ///
    /// Preamble renders the styled header ("server action").
    /// Body shows input parameters, then the full output.
    pub fn for_use_tool(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::UseTool(ut)) = &entry.block else {
            return None;
        };

        let theme = Theme::current();
        let label = Style::default().fg(theme.text_secondary);
        let value = Style::default().fg(theme.text_primary);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Input parameters
        for (k, v) in &ut.input_args {
            lines.push(Line::from(vec![
                Span::styled(format!("{k}: "), label),
                Span::styled(v.clone(), value),
            ]));
        }

        // Output or error
        let content = ut.output.as_deref().or(ut.error.as_deref());
        if let Some(text) = content {
            if !ut.input_args.is_empty() {
                lines.push(Line::from(""));
            }
            let style = if ut.error.is_some() && ut.output.is_none() {
                Style::default().fg(theme.accent_error)
            } else {
                value
            };
            for line in text.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), style)));
            }
        }

        Some(Self::for_static_content(
            entry_id,
            ViewerKind::UseTool,
            lines,
        ))
    }

    /// Create a viewer for a background task (stdout from central store).
    ///
    /// Unlike other viewers that read from the scrollback entry, this one
    /// takes stdout directly and stores the `task_id` for streaming updates.
    pub fn for_bg_task(entry_id: EntryId, task_id: &str, stdout: &str, is_running: bool) -> Self {
        let config = ListPaneConfig {
            follow_enabled: is_running,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::Wrap, is_running, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let theme = Theme::current();
        let items = Self::build_execute_items(Some(stdout).filter(|s| !s.is_empty()), &theme);

        let list_style = ListPaneStyle {
            selection_bg: theme.bg_highlight,
            visual_select_bg: theme.bg_visual,
            uniform_visual_bg: true,
            ..ListPaneStyle::default()
        };

        Self {
            entry_id,
            kind: ViewerKind::BgTask,
            list_state,
            list_style,
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta: Vec::new(),
            last_generation: stdout.len() as u64,
            was_running: is_running,
            bg_task_id: Some(task_id.to_string()),
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        }
    }

    /// Create a viewer for plain text content (e.g., catalog entry).
    pub fn for_plain_text(title: &str, text: &str) -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::Wrap, false, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let theme = Theme::current();
        let title_style = Style::default()
            .fg(theme.accent_user)
            .add_modifier(Modifier::BOLD);
        let style = Style::default().fg(theme.text_primary);

        let mut items = vec![
            ContentLine {
                content: Line::from(Span::styled(title.to_owned(), title_style)),
                plain_text: title.to_owned(),
                id: u64::MAX,
                bg: None,
            },
            ContentLine {
                content: Line::default(),
                plain_text: String::new(),
                id: u64::MAX - 1,
                bg: None,
            },
        ];
        items.extend(text.lines().enumerate().map(|(i, line)| ContentLine {
            content: Line::from(Span::styled(line.to_owned(), style)),
            plain_text: line.to_owned(),
            id: i as u64,
            bg: None,
        }));

        Self {
            entry_id: EntryId::new(0),
            kind: ViewerKind::PlainText,
            list_state,
            list_style: ListPaneStyle {
                uniform_visual_bg: true,
                ..ListPaneStyle::default()
            },
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta: Vec::new(),
            last_generation: 0,
            was_running: false,
            bg_task_id: None,
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        }
    }

    /// Update a BgTask viewer with new stdout content.
    ///
    /// Called from the tick path when stdout in the central store has changed.
    /// Returns `true` if a redraw is needed.
    pub fn tick_bg_task(&mut self, stdout: &str, is_running: bool) -> bool {
        let mut needs_redraw = false;
        let current_len = stdout.len() as u64;

        if current_len != self.last_generation {
            self.last_generation = current_len;
            let theme = Theme::current();
            self.items = Self::build_execute_items(Some(stdout).filter(|s| !s.is_empty()), &theme);
            self.list_state.invalidate_layout();
            needs_redraw = true;
        }

        // Running → finished transition: disable follow
        if self.was_running && !is_running {
            self.was_running = false;
            self.list_state.disable_follow_permanently();
            if let Some(last) = self.items.last() {
                self.list_state.select_by_id(last.stable_id());
            }
            needs_redraw = true;
        }

        needs_redraw
    }

    /// Build content lines from execute stdout output.
    fn build_execute_items(output: Option<&str>, theme: &Theme) -> Vec<ContentLine> {
        let Some(output) = output else {
            return Vec::new();
        };
        let base = ratatui::style::Style::default().fg(theme.text_primary);
        crate::render::terminal_output::render_terminal_lines(output, base)
            .into_iter()
            .enumerate()
            .map(|(i, rl)| ContentLine {
                content: rl.line,
                plain_text: rl.plain,
                id: i as u64,
                bg: Some(theme.bg_dark),
            })
            .collect()
    }

    /// Create a viewer for an edit block (diff content).
    pub fn for_edit(entry_id: EntryId, entry: &ScrollbackEntry) -> Option<Self> {
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
            return None;
        };
        if edit.hunks.is_empty() {
            return None;
        }

        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::Wrap, false, config);
        list_state.set_clipboard_provider(Box::new(SystemClipboard));

        let theme = Theme::current();
        let (items, diff_meta) = Self::build_edit_items(edit, &theme);

        Some(Self {
            entry_id,
            kind: ViewerKind::Edit,
            list_state,
            list_style: ListPaneStyle {
                uniform_visual_bg: true,
                ..ListPaneStyle::default()
            },
            items,
            last_content_area: Rect::default(),

            raw_toggle_pending: false,
            copy_meta_pending: false,
            copy_content_pending: false,
            diff_meta,
            last_generation: 0,
            was_running: false,
            bg_task_id: None,
            last_theme: Theme::current_kind(),
            modal: ModalWindowState::new(),
            prepend_items: Vec::new(),
            text_drag: None,
            drag_copy_text: None,
            cached_unified: Vec::new(),
        })
    }

    /// Build content lines and diff metadata from edit block diff hunks.
    fn build_edit_items(
        edit: &crate::scrollback::blocks::EditToolCallBlock,
        theme: &Theme,
    ) -> (Vec<ContentLine>, Vec<Option<DiffLineMeta>>) {
        use crate::scrollback::blocks::DiffRenderConfig;

        let config = DiffRenderConfig::default();
        // Block-owned dispatch so the viewer paints the same highlight phase
        // (incl. the file-scoped upgrade) as the scrollback output.
        let rendered = edit.render_diff_lines(
            theme, 500, // wide width — NoWrap mode
            &config,
        );

        // Build a flat list of DiffLine references from all hunks, interleaving
        // None for separator lines (which render_diff_lines inserts).
        // The rendered output has: [hunk0 lines...] [separator] [hunk1 lines...] ...
        let mut meta_source: Vec<Option<&echo_agent_pager_diff::DiffLine>> = Vec::new();
        for (i, hunk) in edit.hunks.iter().enumerate() {
            if i > 0 && !config.hunk_separator.is_empty() {
                meta_source.push(None); // separator line
            }
            for diff_line in hunk {
                meta_source.push(Some(diff_line));
                // Wrapped lines: render_diff_hunk_highlighted may produce multiple
                // DiffLineOutput per DiffLine. For now, assume 1:1 mapping since
                // we use width=500 (very wide, unlikely to wrap).
            }
        }

        let mut items = Vec::with_capacity(rendered.len());
        let mut diff_meta = Vec::with_capacity(rendered.len());

        for (i, dl) in rendered.into_iter().enumerate() {
            let plain: String = dl.line.spans.iter().map(|s| s.content.as_ref()).collect();
            items.push(ContentLine {
                content: dl.line,
                plain_text: plain,
                id: i as u64,
                bg: dl.background,
            });
            diff_meta.push(meta_source.get(i).copied().flatten().map(|d| DiffLineMeta {
                tag: d.tag,
                text: d.text.clone(),
                lo: d.lo,
                ln: d.ln,
            }));
        }

        (items, diff_meta)
    }

    /// Extract pre-wrap lines from a markdown block (thinking or agent message).
    fn extract_markdown_lines(block: &RenderBlock) -> Option<Vec<Line<'static>>> {
        match block {
            RenderBlock::Thinking(b) => Some(b.content().pre_wrap_lines()),
            RenderBlock::AgentMessage(b) => Some(b.content().pre_wrap_lines()),
            _ => None,
        }
    }

    /// Extract a change-detection counter from the block.
    ///
    /// For markdown blocks: content generation counter.
    /// For execute blocks: output byte length (no generation counter available).
    fn extract_generation(block: &RenderBlock) -> Option<u64> {
        match block {
            RenderBlock::Thinking(b) => Some(b.content().generation()),
            RenderBlock::AgentMessage(b) => Some(b.content().generation()),
            RenderBlock::ToolCall(ToolCallBlock::Execute(b)) => {
                Some(b.output.as_ref().map_or(0, |o| o.len()) as u64)
            }
            _ => None,
        }
    }

    /// Extract the line source map from a markdown block.
    fn extract_line_source_map(block: &RenderBlock) -> Option<Vec<usize>> {
        match block {
            RenderBlock::Thinking(b) => Some(b.content().line_source_map()),
            RenderBlock::AgentMessage(b) => Some(b.content().line_source_map()),
            _ => None,
        }
    }

    /// Rebuild items from the block's current content.
    ///
    /// Called after raw mode toggle or when content changes during streaming.
    /// Invalidates the ListPane layout cache since item content/heights may differ.
    pub fn rebuild_items(&mut self, entry: &ScrollbackEntry) {
        match self.kind {
            ViewerKind::Markdown => {
                if let Some(lines) = Self::extract_markdown_lines(&entry.block) {
                    self.items = ContentLine::from_lines(&lines);
                    self.list_state.invalidate_layout();
                }
            }
            ViewerKind::Execute => {
                if let RenderBlock::ToolCall(ToolCallBlock::Execute(exec)) = &entry.block {
                    let theme = Theme::current();
                    self.items = Self::build_execute_items(exec.output.as_deref(), &theme);
                    self.list_state.invalidate_layout();
                }
            }
            ViewerKind::Edit => {}
            ViewerKind::BgTask => {}
            ViewerKind::Read
            | ViewerKind::Grep
            | ViewerKind::WebFetch
            | ViewerKind::WebSearch
            | ViewerKind::IntegrationSearch
            | ViewerKind::UseTool
            | ViewerKind::PlainText => {}
        }
    }

    /// Look up the source line number for a given stable_id (pre-wrap line index).
    ///
    /// Used by the caller to capture cursor position before a raw toggle.
    pub fn source_line_for_id(block: &RenderBlock, id: u64) -> Option<usize> {
        Self::extract_line_source_map(block).and_then(|map| map.get(id as usize).copied())
    }

    /// Jump the cursor to the first rendered line matching the given source line.
    ///
    /// Called after raw toggle + rebuild to restore cursor position.
    pub fn jump_to_source_line(&mut self, entry: &ScrollbackEntry, target: Option<usize>) {
        let Some(target_source) = target else {
            return;
        };
        if let Some(new_map) = Self::extract_line_source_map(&entry.block) {
            let new_idx = new_map
                .iter()
                .position(|&sl| sl >= target_source)
                .unwrap_or(0);
            self.list_state.select_by_id(new_idx as u64);
        }
    }

    /// Update viewer state on tick (streaming content, follow mode).
    ///
    /// Returns `true` if a redraw is needed.
    pub fn tick(&mut self, entry: &ScrollbackEntry) -> bool {
        let mut needs_redraw = false;

        // Check if content changed via generation counter.
        // Generation is bumped on every push_chunk, finish, or set_raw_mode.
        if let Some(current_gen) = Self::extract_generation(&entry.block)
            && current_gen != self.last_generation
        {
            self.last_generation = current_gen;
            self.rebuild_items(entry);
            needs_redraw = true;
        }

        // On running→finished transition: disable follow permanently,
        // select the last item so the user has a cursor at the bottom.
        if self.was_running && !entry.is_running {
            self.was_running = false;
            self.list_state.disable_follow_permanently();
            // Select the last item by stable_id (layout may be stale after
            // invalidate_layout, so we can't use select_last which reads
            // layout.item_count). select_by_id is resolved on next prepare_layout.
            if let Some(last) = self.items.last() {
                self.list_state.select_by_id(last.stable_id());
            }
            needs_redraw = true;
        }

        needs_redraw
    }

    /// Build shortcuts bar hints for this viewer.
    pub fn shortcuts_hints(&self) -> Vec<HintItem> {
        let mut hints = vec![
            HintItem::new(crate::key!(Esc), "close"),
            HintItem::new(crate::key!('/'), "search"),
            HintItem::new(crate::key!('f'), "filter"),
            HintItem::new(crate::key!('v'), "select"),
            HintItem::new(crate::key!('w'), "wrap"),
        ];
        match self.kind {
            ViewerKind::Markdown => {
                hints.push(HintItem::new(crate::key!('r'), "raw"));
            }
            ViewerKind::Execute => {
                hints.push(HintItem::new(crate::key!('Y'), "copy cmd"));
            }
            ViewerKind::Edit => {
                hints.push(HintItem::new(crate::key!('Y'), "copy path"));
            }
            ViewerKind::WebFetch => {
                hints.push(HintItem::new(crate::key!('Y'), "copy url"));
            }
            ViewerKind::WebSearch => {
                hints.push(HintItem::new(crate::key!('Y'), "copy query"));
            }
            ViewerKind::Read => {
                hints.push(HintItem::new(crate::key!('Y'), "copy path"));
            }
            ViewerKind::Grep => {
                hints.push(HintItem::new(crate::key!('Y'), "copy pattern"));
            }
            ViewerKind::BgTask => {}
            ViewerKind::IntegrationSearch | ViewerKind::UseTool | ViewerKind::PlainText => {}
        }
        hints
    }

    // -- Input handling ------------------------------------------------------

    /// Check if a key is a close signal (Esc/q/Ctrl-F).
    ///
    /// Separated from `handle_key` so the caller can close the viewer
    /// before routing the key (avoids borrow conflicts).
    pub fn is_close_key(&self, key: &KeyEvent) -> bool {
        // Ctrl-F: close viewer (toggle off)
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        // Esc / q: close viewer (when input bar and visual select are not active)
        self.list_state.input_mode().is_none()
            && !self.list_state.visual_mode
            && (matches!(key.code, KeyCode::Esc)
                || (key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::NONE))
    }

    /// Handle a key event while the viewer is focused.
    ///
    /// Returns `true` if the key was consumed, `false` if it should bubble up.
    /// Close keys should be checked via `is_close_key` before calling this.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        // r: toggle raw mode (markdown blocks only) — handled by caller
        if self.kind == ViewerKind::Markdown
            && key.code == KeyCode::Char('r')
            && key.modifiers == KeyModifiers::NONE
            && self.list_state.input_mode().is_none()
        {
            self.raw_toggle_pending = true;
            return true;
        }

        // Y: copy meta (execute: command, edit: path) — handled by caller
        if matches!(
            self.kind,
            ViewerKind::Execute
                | ViewerKind::Edit
                | ViewerKind::Read
                | ViewerKind::Grep
                | ViewerKind::WebFetch
                | ViewerKind::WebSearch
        ) && key.code == KeyCode::Char('Y')
            && key.modifiers == KeyModifiers::SHIFT
            && self.list_state.input_mode().is_none()
        {
            self.copy_meta_pending = true;
            return true;
        }

        // y: copy patch format (edit blocks) — intercept before ListPane.
        // Non-visual: copies full patch (same as scrollback `y`).
        // Visual: copies patch for the selected range.
        if self.kind == ViewerKind::Edit
            && key.code == KeyCode::Char('y')
            && key.modifiers == KeyModifiers::NONE
            && self.list_state.input_mode().is_none()
        {
            self.copy_content_pending = true;
            return true;
        }

        // Route to ListPaneState — propagate whether the key was consumed.
        // Pass the same unified vec the layout was prepared against so
        // selection / scroll math stays in bounds. Any consumed key
        // (navigation, search, copy, etc.) also clears a sticky mouse
        // text selection so the highlight doesn't linger across actions.
        self.rebuild_unified_cache();
        let consumed = self.list_state.handle_key_event(key, &self.cached_unified);
        if consumed {
            self.text_drag = None;
        }
        consumed
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.rebuild_unified_cache();
        let consumed = self.list_state.handle_paste(text, &self.cached_unified);
        if consumed {
            self.text_drag = None;
        }
        consumed
    }

    /// Generate a patch string from the diff metadata in the given item range.
    ///
    /// Returns `None` if this isn't an edit viewer or the range has no diff lines.
    pub fn patch_from_range(&self, path: &str, range: std::ops::Range<usize>) -> Option<String> {
        if self.diff_meta.is_empty() {
            return None;
        }

        let mut out = String::new();
        out.push_str(&format!("--- a/{path}\n"));
        out.push_str(&format!("+++ b/{path}\n"));

        // Collect non-None entries in the range
        let entries: Vec<&DiffLineMeta> = range
            .filter_map(|i| self.diff_meta.get(i).and_then(|m| m.as_ref()))
            .collect();
        if entries.is_empty() {
            return None;
        }

        // Compute hunk header
        let old_start = entries
            .iter()
            .filter(|e| e.tag != similar::ChangeTag::Insert)
            .map(|e| e.lo)
            .next()
            .unwrap_or(1);
        let new_start = entries
            .iter()
            .filter(|e| e.tag != similar::ChangeTag::Delete)
            .map(|e| e.ln)
            .next()
            .unwrap_or(1);
        let old_count = entries
            .iter()
            .filter(|e| e.tag != similar::ChangeTag::Insert)
            .count();
        let new_count = entries
            .iter()
            .filter(|e| e.tag != similar::ChangeTag::Delete)
            .count();

        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));

        for entry in &entries {
            let prefix = match entry.tag {
                similar::ChangeTag::Equal => ' ',
                similar::ChangeTag::Insert => '+',
                similar::ChangeTag::Delete => '-',
            };
            let text = entry.text.trim_end_matches(['\r', '\n']);
            out.push(prefix);
            out.push_str(text);
            out.push('\n');
        }

        Some(out)
    }

    /// Process pending actions (copy meta, copy content) and return text to copy.
    ///
    /// Call after `handle_key` or `handle_mouse`. The caller is responsible for
    /// clipboard operations and toast display. Returns `None` if no copy pending.
    pub fn process_pending_copy(&mut self, entry: &ScrollbackEntry) -> Option<String> {
        // Y — copy command (execute blocks)
        if self.copy_meta_pending {
            self.copy_meta_pending = false;
            return entry.block.copy_meta().filter(|t| !t.is_empty());
        }

        // y — copy content (edit blocks: patch format; others: block content)
        if self.copy_content_pending {
            self.copy_content_pending = false;
            let result = if self.kind == ViewerKind::Edit {
                // Patch from copy range (visual selection or current line).
                // copy_range() returns indices into the unified vec (prepend +
                // items), but diff_meta is parallel to items only. Adjust by
                // subtracting the prepend offset so we index diff_meta correctly.
                let copy_range = self.list_state.copy_range();
                let path = match &entry.block {
                    RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) => Some(&edit.path),
                    _ => None,
                };
                match (copy_range, path) {
                    (Some(range), Some(path)) => {
                        let offset = self.prepend_items.len();
                        let adj_start = range.start.saturating_sub(offset);
                        let adj_end = range.end.saturating_sub(offset);
                        if adj_start < adj_end {
                            self.patch_from_range(path, adj_start..adj_end)
                        } else {
                            None
                        }
                    }
                    _ => entry.block.copy_text(entry.raw),
                }
            } else {
                entry.block.copy_text(entry.raw)
            };
            self.list_state.exit_visual_mode();
            return result.filter(|t| !t.is_empty());
        }

        None
    }

    /// Rebuild the cached unified items vec (preamble + body). The cache
    /// retains its allocation across calls so repeated rebuilds within a
    /// frame avoid fresh heap allocations. Callers that hold `&mut self`
    /// call this first, then use `&self.cached_unified` via a disjoint
    /// field borrow alongside `&mut self.list_state` etc.
    fn rebuild_unified_cache(&mut self) {
        self.cached_unified.clear();
        self.cached_unified
            .extend(self.prepend_items.iter().cloned());
        self.cached_unified.extend(self.items.iter().cloned());
    }

    /// Handle mouse scroll in the viewer area.
    pub fn handle_scroll(&mut self, lines: i32) {
        self.rebuild_unified_cache();
        self.list_state.scroll_lines(lines, &self.cached_unified);
    }

    /// Handle mouse click/drag in the viewer area.
    ///
    /// Drag-to-highlight: a left-button drag inside the content area paints
    /// a character-precise selection (same model as the scrollback pane).
    /// Anchor and head are STABLE references (item index + char offset in
    /// the item's logical line), so the selection survives scrolling and
    /// the auto-copy on release includes any content the drag covered even
    /// if it's now off-screen. The selection stays visible after release
    /// until the next click.
    pub fn handle_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind as MEK};
        self.rebuild_unified_cache();
        let pane_area = self.last_content_area;
        let scrollbar_x_range = self
            .list_state
            .scrollbar_area()
            .map(|sb| (sb.x, sb.x + sb.width));

        // Is this position inside the content area but not on the scrollbar?
        let in_content = |c: u16, r: u16| -> bool {
            r >= pane_area.y
                && r < pane_area.y + pane_area.height
                && c >= pane_area.x
                && c < pane_area.x + pane_area.width
                && match scrollbar_x_range {
                    Some((lo, hi)) => !(c >= lo && c < hi),
                    None => true,
                }
        };

        match kind {
            MEK::Down(MouseButton::Left) if in_content(col, row) => {
                // Start a fresh text selection anchored at the clicked
                // character. Also clear any keyboard visual-mode highlight
                // so the user only sees the new mouse selection.
                self.list_state.exit_visual_mode();
                if let Some(ep) = self.screen_to_endpoint(col, row, &self.cached_unified) {
                    self.text_drag = Some(TextDrag {
                        anchor: ep,
                        head: ep,
                        active: true,
                    });
                }
                true
            }
            MEK::Drag(MouseButton::Left) => {
                if let Some(drag) = self.text_drag
                    && drag.active
                {
                    // Auto-scroll when the cursor drags above or below the
                    // content area (same idea as scrollback drag_autoscroll).
                    // Speed scales with distance from the edge (1..5 rows).
                    if row < pane_area.y {
                        let distance = pane_area.y.saturating_sub(row) as i32;
                        self.list_state
                            .scroll_lines(-distance.clamp(1, 5), &self.cached_unified);
                    } else if row >= pane_area.y + pane_area.height {
                        let distance =
                            (row.saturating_sub(pane_area.y + pane_area.height) + 1) as i32;
                        self.list_state
                            .scroll_lines(distance.clamp(1, 5), &self.cached_unified);
                    }

                    // Clamp the cursor to the content area then map to an
                    // item endpoint so the selection extends as we scroll.
                    let max_c = pane_area.x + pane_area.width.saturating_sub(1);
                    let max_r = pane_area.y + pane_area.height.saturating_sub(1);
                    let cc = col.clamp(pane_area.x, max_c);
                    let rr = row.clamp(pane_area.y, max_r);
                    if let Some(ep) = self.screen_to_endpoint(cc, rr, &self.cached_unified)
                        && let Some(d) = self.text_drag.as_mut()
                    {
                        d.head = ep;
                    }
                    return true;
                }
                false
            }
            MEK::Up(MouseButton::Left) => {
                if let Some(drag) = self.text_drag
                    && drag.active
                {
                    // Update head to the release position — the last
                    // Drag(Left) may trail the actual release by a few
                    // characters, causing 1-2 missing words at the end.
                    let mut final_drag = drag;
                    let max_c = pane_area.x + pane_area.width.saturating_sub(1);
                    let max_r = pane_area.y + pane_area.height.saturating_sub(1);
                    let cc = col.clamp(pane_area.x, max_c);
                    let rr = row.clamp(pane_area.y, max_r);
                    if let Some(ep) = self.screen_to_endpoint(cc, rr, &self.cached_unified) {
                        final_drag.head = ep;
                    }

                    if final_drag.is_non_empty() {
                        self.drag_copy_text = self.text_for_drag(final_drag, &self.cached_unified);
                    }
                    self.text_drag = None;
                    return true;
                }
                // Click without drag — just clear.
                self.text_drag = None;
                true
            }
            // Any other button-down (including a left-click outside the
            // content area) clears a sticky selection so the highlight
            // doesn't linger after the user moves on.
            MEK::Down(_) => {
                self.text_drag = None;
                self.list_state
                    .handle_mouse_event(kind, col, row, pane_area, &self.cached_unified)
            }
            _ => {
                self.list_state
                    .handle_mouse_event(kind, col, row, pane_area, &self.cached_unified)
            }
        }
    }

    /// Translate a screen position inside the content area into a stable
    /// `TextEndpoint` (item index + char/display-column offset within the
    /// item's logical line text).
    ///
    /// Returns `None` if the position doesn't map to any item (e.g. blank
    /// space below the last visual row of the content). The result is
    /// clamped: clicks beyond the end of an item's last sub-row map to
    /// `col = end_of_logical_line`, and clicks past the rightmost char on
    /// a sub-row map to that sub-row's end column.
    fn screen_to_endpoint(
        &self,
        col: u16,
        row: u16,
        items: &[ContentLine],
    ) -> Option<TextEndpoint> {
        let pane = self.last_content_area;
        if row < pane.y || col < pane.x {
            return None;
        }
        let virtual_y = self.list_state.scroll_offset() + (row - pane.y) as usize;
        let item_idx = self.list_state.layout().item_at_y(virtual_y)?;
        let item = items.get(item_idx)?;
        let item_top = self.list_state.layout().virtual_y(item_idx);
        let sub_row = virtual_y.saturating_sub(item_top) as u16;
        let col_in_sub = col.saturating_sub(pane.x);

        // Re-wrap this item's content at the same width the layout used.
        // Use joiners to account for spaces dropped at each break point —
        // without them, absolute_col drifts by 1+ chars per wrap break.
        let wrap_w = self.effective_wrap_width();
        let (wrapped, joiners) = self.wrap_item_with_joiners(item, wrap_w);
        let mut absolute_col: u16 = 0;
        for (i, line) in wrapped.iter().enumerate() {
            // Add joiner width (spaces dropped at the break point).
            if i > 0
                && let Some(Some(j)) = joiners.get(i)
            {
                absolute_col = absolute_col
                    .saturating_add(crate::scrollback::types::str_display_cells(j.as_str()) as u16);
            }
            let line_w = line_display_width_u16(line);
            if i == sub_row as usize {
                if col_in_sub >= line_w && i + 1 < wrapped.len() {
                    // Cursor at/past the right edge of a non-last sub-row:
                    // snap to end of logical line.
                    let text = item.copy_text();
                    let total = crate::scrollback::types::str_display_cells(&text)
                        .min(u16::MAX as usize) as u16;
                    return Some(TextEndpoint {
                        item_idx,
                        col: total,
                    });
                }
                // Endpoints are logical (copy slices logical text), so map the
                // clicked visual cell back to its logical column in this sub-row.
                let sub_logical = crate::scrollback::types::line_plain_text(line);
                let logical_in_sub = crate::render::bidi::visual_col_to_logical_col(
                    &sub_logical,
                    col_in_sub.min(line_w) as usize,
                ) as u16;
                absolute_col = absolute_col.saturating_add(logical_in_sub);
                return Some(TextEndpoint {
                    item_idx,
                    col: absolute_col,
                });
            }
            absolute_col = absolute_col.saturating_add(line_w);
        }
        // sub_row past the wrapped output → clamp to end of last sub-row.
        // (This also handles items shorter than the layout item_height —
        // shouldn't happen, but keeps the click meaningful.)
        Some(TextEndpoint {
            item_idx,
            col: absolute_col,
        })
    }

    /// Effective wrap width used by the ListPane for this frame: pane width
    /// minus the scrollbar gutter when a scrollbar is shown.
    fn effective_wrap_width(&self) -> u16 {
        let pane = self.last_content_area;
        let pane_right = pane.x + pane.width;
        match self.list_state.scrollbar_area() {
            // Scrollbar starts at sb.x; usable text width is sb.x - pane.x.
            Some(sb) if sb.x >= pane.x && sb.x < pane_right => sb.x - pane.x,
            _ => pane.width,
        }
    }

    /// Wrap one item's content, returning wrapped lines and the per-break
    /// joiners (spaces dropped at each break point). The joiner widths
    /// are needed to correctly map screen positions to display-column
    /// offsets in the original unwrapped text.
    fn wrap_item_with_joiners(
        &self,
        item: &ContentLine,
        width: u16,
    ) -> (Vec<Line<'static>>, Vec<Option<String>>) {
        if width == 0 {
            return (vec![item.content.clone()], vec![None]);
        }
        let (wrapped, joiners) =
            crate::render::wrapping::word_wrap_line_with_joiners(&item.content, width as usize);
        if wrapped.is_empty() {
            (vec![item.content.clone()], vec![None])
        } else {
            let lines = wrapped
                .iter()
                .map(crate::render::line_utils::line_to_static)
                .collect();
            (lines, joiners)
        }
    }

    /// First cell of the grapheme occupying `col`, or `col` itself past the text.
    fn col_at_char_start(text: &str, col: u16) -> u16 {
        crate::scrollback::types::grapheme_cells_at(text, col).map_or(col, |cells| cells.start)
    }

    /// Extract the selected text from the unified items list.
    /// Column-precise: slices partial lines at the start/end of the
    /// selection, full lines in the middle.
    fn text_for_drag(&self, drag: TextDrag, items: &[ContentLine]) -> Option<String> {
        let (start, end) = drag.ordered();
        let mut out = String::new();
        let mut wrote_any = false;
        for idx in start.item_idx..=end.item_idx {
            let Some(item) = items.get(idx) else {
                break;
            };
            let text = item.copy_text();
            let slice = if start.item_idx == end.item_idx {
                // Single-item: slice [start .. end_inclusive]
                let lo = Self::col_at_char_start(&text, start.col);
                let hi = crate::scrollback::types::col_past_grapheme(&text, end.col);
                crate::scrollback::types::slice_display_cols(&text, lo, hi)
            } else if idx == start.item_idx {
                // First item: from start to end of line
                let lo = Self::col_at_char_start(&text, start.col);
                crate::scrollback::types::slice_display_cols(&text, lo, u16::MAX)
            } else if idx == end.item_idx {
                // Last item: from beginning to end_inclusive
                let hi = crate::scrollback::types::col_past_grapheme(&text, end.col);
                crate::scrollback::types::slice_display_cols(&text, 0, hi)
            } else {
                // Middle items: full line
                text
            };
            if wrote_any {
                out.push('\n');
            }
            out.push_str(&slice);
            wrote_any = true;
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// Paint the active / sticky text selection overlay onto `buf`.
    ///
    /// Called by the modal renderer AFTER `render_content` has drawn the
    /// items. Walks the visible portion of the selection (off-screen
    /// portions are hidden, but they're still in the copy text), inverts
    /// fg/bg on every cell that falls inside the per-sub-row column range.
    pub fn render_text_drag_overlay(&self, buf: &mut ratatui::buffer::Buffer) {
        let Some(drag) = self.text_drag else {
            return;
        };
        if !drag.is_non_empty() {
            return;
        }
        let theme = Theme::current();
        let pane = self.last_content_area;
        if pane.width == 0 || pane.height == 0 {
            return;
        }
        let (start, end) = drag.ordered();

        // Advance end past the character under the cursor so the
        // highlight includes the pointed-at character (matches copy).
        let end_col_hi = self
            .cached_unified
            .get(end.item_idx)
            .map(|item| crate::scrollback::types::col_past_grapheme(&item.copy_text(), end.col))
            .unwrap_or(end.col);
        let end = TextEndpoint {
            item_idx: end.item_idx,
            col: end_col_hi,
        };

        // Floor the start onto its grapheme so the overlay matches the copy.
        let start_col_lo = self
            .cached_unified
            .get(start.item_idx)
            .map(|item| Self::col_at_char_start(&item.copy_text(), start.col))
            .unwrap_or(start.col);
        let start = TextEndpoint {
            item_idx: start.item_idx,
            col: start_col_lo,
        };

        let scroll = self.list_state.scroll_offset();
        let pane_top = pane.y;
        let pane_bottom = pane.y + pane.height;
        let wrap_w = self.effective_wrap_width();

        for idx in start.item_idx..=end.item_idx {
            let Some(item) = self.cached_unified.get(idx) else {
                break;
            };
            let item_top = self.list_state.layout().virtual_y(idx);
            let item_h = self.list_state.layout().item_height(idx) as usize;
            // Skip items entirely above / below the viewport.
            if item_top + item_h <= scroll {
                continue;
            }
            if item_top >= scroll + pane.height as usize {
                break;
            }
            let (wrapped, joiners) = self.wrap_item_with_joiners(item, wrap_w);
            // Selection covers chars [item_lo .. item_hi) of this item's
            // logical line.
            let (item_lo, item_hi) = if start.item_idx == end.item_idx {
                (start.col, end.col)
            } else if idx == start.item_idx {
                (start.col, u16::MAX)
            } else if idx == end.item_idx {
                (0, end.col)
            } else {
                (0, u16::MAX)
            };
            // Walk this item's sub-rows. acc_col tracks the absolute
            // column in the original text, including joiner widths.
            let mut acc_col: u16 = 0;
            for (sub_i, line) in wrapped.iter().enumerate() {
                if sub_i > 0
                    && let Some(Some(j)) = joiners.get(sub_i)
                {
                    acc_col = acc_col.saturating_add(crate::scrollback::types::str_display_cells(
                        j.as_str(),
                    ) as u16);
                }
                let line_w = line_display_width_u16(line);
                let sub_start = acc_col;
                let sub_end = acc_col.saturating_add(line_w);
                acc_col = sub_end;
                // Compute selection overlap in absolute item-cols.
                let sel_start = item_lo.max(sub_start);
                let sel_end = item_hi.min(sub_end);
                if sel_end <= sel_start {
                    continue;
                }
                // Translate to screen coordinates.
                let virtual_y = item_top + sub_i;
                if virtual_y < scroll {
                    continue;
                }
                let screen_y = pane_top + (virtual_y - scroll) as u16;
                if screen_y >= pane_bottom {
                    break;
                }
                let local_lo = sel_start - sub_start;
                let local_hi = sel_end - sub_start;
                // Selection is logical; map it to the visual cells that hold it.
                let sub_logical = crate::scrollback::types::line_plain_text(line);
                let visual_ranges = if crate::render::bidi::is_enabled()
                    && crate::render::bidi::needs_bidi(&sub_logical)
                {
                    crate::render::bidi::logical_cols_to_visual(
                        &sub_logical,
                        local_lo as usize,
                        local_hi as usize,
                    )
                } else {
                    vec![(local_lo as usize, local_hi as usize)]
                };
                let pane_right = pane.x + pane.width;
                for (vlo, vhi) in visual_ranges {
                    let x_lo = pane.x.saturating_add(vlo as u16).min(pane_right);
                    let x_hi = pane.x.saturating_add(vhi as u16).min(pane_right);
                    for x in x_lo..x_hi {
                        if let Some(cell) = buf.cell_mut((x, screen_y)) {
                            crate::scrollback::text_selection::apply_selection_highlight(
                                &theme, cell,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Painted width of a `Line` in terminal cells, clamped to `u16::MAX`. Counts
/// per-grapheme cells (like `types::str_display_cells`) so it agrees with
/// `visual_col_to_logical_col` on ligature rows (Arabic lam-alef).
fn line_display_width_u16(line: &Line<'_>) -> u16 {
    let w: usize = line
        .spans
        .iter()
        .map(|s| crate::scrollback::types::str_display_cells(s.content.as_ref()))
        .sum();
    w.min(u16::MAX as usize) as u16
}

impl BlockViewerPane {
    // -- Rendering -----------------------------------------------------------

    /// Render the viewer content into the given area (provided by modal chrome).
    ///
    /// `content_area` is the inner content rect returned by `render_modal_window`.
    /// `entry` is the scrollback entry being viewed (for theme rebuild).
    /// `prepend_lines` are pre-wrapped header lines to render at the top of
    /// the scrollable content (preamble + a blank separator). They get their
    /// own stable IDs in the high half of the u64 space so they don't
    /// conflict with content item IDs.
    pub fn render_content(
        &mut self,
        content_area: Rect,
        buf: &mut Buffer,
        entry: &ScrollbackEntry,
        focused: bool,
        prepend_lines: &[Line<'static>],
    ) {
        let theme = Theme::current();

        // Detect theme switch and rebuild items + list style.
        let current_theme = Theme::current_kind();
        if current_theme != self.last_theme {
            self.last_theme = current_theme;
            self.list_style = match self.kind {
                ViewerKind::BgTask | ViewerKind::Execute => ListPaneStyle {
                    selection_bg: theme.bg_highlight,
                    visual_select_bg: theme.bg_visual,
                    uniform_visual_bg: true,
                    ..ListPaneStyle::default()
                },
                _ => ListPaneStyle {
                    uniform_visual_bg: true,
                    ..ListPaneStyle::default()
                },
            };
            match self.kind {
                ViewerKind::Execute => {
                    let output = entry.block.copy_text(entry.raw);
                    self.items = Self::build_execute_items(
                        output.as_deref().filter(|s| !s.is_empty()),
                        &theme,
                    );
                    self.list_state.invalidate_layout();
                }
                ViewerKind::BgTask => {
                    self.last_generation = u64::MAX;
                }
                _ => {}
            }
        }

        self.last_content_area = content_area;

        // Cache prepend lines as ContentLines on self so the input handlers
        // (scroll / mouse / key) can rebuild the same unified vec the
        // ListPane saw — otherwise they index a smaller `items` vec and
        // panic when the cursor / scroll math points past its end.
        // Header IDs are placed in the high u64 range so they never collide
        // with content IDs (which start at 0 and grow upward).
        self.prepend_items = prepend_lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                ContentLine {
                    content: line.clone(),
                    plain_text: plain,
                    id: u64::MAX - i as u64,
                    bg: line.style.bg,
                }
            })
            .collect();
        self.rebuild_unified_cache();

        if content_area.height > 0 && content_area.width > 0 && !self.cached_unified.is_empty() {
            let likely_scrollbar = self.cached_unified.len() > content_area.height as usize;
            let render_area = if likely_scrollbar {
                Rect {
                    width: content_area.width + SCROLLBAR_TOTAL_COLS.min(2),
                    ..content_area
                }
            } else {
                content_area
            };

            self.list_state.prepare_layout(
                &self.cached_unified,
                render_area.width,
                render_area.height,
            );

            let render_area = if !likely_scrollbar
                && self.list_state.total_height() > render_area.height as usize
            {
                let wider = Rect {
                    width: content_area.width + SCROLLBAR_TOTAL_COLS.min(2),
                    ..content_area
                };
                self.list_state
                    .prepare_layout(&self.cached_unified, wider.width, wider.height);
                wider
            } else {
                render_area
            };

            ListPane::new(&self.cached_unified)
                .focused(focused)
                .style(self.list_style)
                .render(render_area, buf, &mut self.list_state);
        }
    }
}
