//! Line viewer popup for selecting line ranges from a file.
//!
//! A centered modal overlay showing syntax-highlighted file content with
//! line numbers. Backed by [`ListPaneState`] for navigation, visual selection,
//! and search. Used to build `@foo/bar.rs:10-12` line references.
//!
//! ## Lifecycle
//!
//! 1. Opened via `:` in dropdown, `Ctrl-L` on element, or `<left>:` after element.
//! 2. User navigates with j/k, searches with `/`, selects range with `v`.
//! 3. **Enter** confirms → element updated with line range, undo group closed.
//! 4. **Esc** cancels → undo group cancelled, reverts to pre-viewer state.

use std::ops::Range;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{StatefulWidget, Widget};
use syntect::easy::HighlightLines;

use crate::render::scrollbar::SCROLLBAR_TOTAL_COLS;
use crate::render::wrapping::word_wrap_line;
use crate::scrollback::blocks::markdown_content::MarkdownContent;
use crate::scrollback::blocks::mermaid_content::{MermaidDisplay, mermaid_display};
use crate::scrollback::render::DiagramAffordancePlacement;
use crate::syntax::get_syntect;
use crate::theme::Theme;
use crate::views::list_pane::{
    ListItem, ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode,
};

use echo_agent_ratatui_textarea::ElementId;

/// Stable ids for mermaid affordance rows (above source lines and comments).
const MERMAID_AFFORDANCE_ID_BASE: u64 = 2_000_000;

// ── Line item ───────────────────────────────────────────────────────────

/// A single source line for the line viewer.
///
/// In normal mode, each item has one `content` line (syntax-highlighted source).
/// In markdown mode, `rendered_lines` holds the rendered markdown output for this
/// source line (may be multiple visual lines, e.g., table with borders). The item
/// uses custom `render()` and `desired_height()` for multi-line display.
#[derive(Clone)]
pub struct SourceLine {
    /// 1-based line number (for display and `@file:N-M` references).
    line_number: usize,
    /// Unique item ID for ListPane selection tracking. In normal mode this
    /// equals `line_number`. In markdown mode, source lines can repeat
    /// (e.g., table borders) so we use a monotonic counter instead.
    item_id: u64,
    /// Styled content (syntax highlighted). Used in normal mode.
    content: Line<'static>,
    /// Prefix: right-aligned line number (dim — default).
    prefix: Line<'static>,
    /// Prefix for visual selection range (medium brightness).
    prefix_in_selection: Line<'static>,
    /// Prefix for the cursor line (brightest).
    prefix_cursor: Line<'static>,
    /// Blank prefix for continuation lines in multi-line items.
    prefix_blank: Line<'static>,
    /// Plain text for search matching.
    plain_text: String,
    /// Rendered markdown lines (empty in normal mode).
    rendered_lines: Vec<Line<'static>>,
    /// Per-rendered-line background color (for code blocks).
    rendered_bgs: Vec<Option<Color>>,
    /// Whether this source line has a comment attached (for highlight).
    pub commented: bool,
}

impl SourceLine {
    fn new(
        line_number: usize,
        content: Line<'static>,
        plain_text: String,
        max_digits: usize,
    ) -> Self {
        let (prefix, prefix_in_selection, prefix_cursor, prefix_blank) =
            build_prefixes(line_number, max_digits);
        Self {
            line_number,
            item_id: line_number as u64,
            content,
            prefix,
            prefix_in_selection,
            prefix_cursor,
            prefix_blank,
            plain_text,
            rendered_lines: Vec::new(),
            rendered_bgs: Vec::new(),
            commented: false,
        }
    }

    fn new_markdown(
        item_id: u64,
        line_number: usize,
        rendered_lines: Vec<Line<'static>>,
        rendered_bgs: Vec<Option<Color>>,
        plain_text: String,
        max_digits: usize,
    ) -> Self {
        let (prefix, prefix_in_selection, prefix_cursor, prefix_blank) =
            build_prefixes(line_number, max_digits);
        Self {
            line_number,
            item_id,
            content: Line::default(),
            prefix,
            prefix_in_selection,
            prefix_cursor,
            prefix_blank,
            plain_text,
            rendered_lines,
            rendered_bgs,
            commented: false,
        }
    }

    fn is_markdown(&self) -> bool {
        !self.rendered_lines.is_empty()
    }
}

fn build_prefixes(
    line_number: usize,
    max_digits: usize,
) -> (Line<'static>, Line<'static>, Line<'static>, Line<'static>) {
    let theme = Theme::current();
    let num_str = format!("{:>width$} ", line_number, width = max_digits);
    let blank_str = " ".repeat(num_str.len());
    let prefix = Line::from(Span::styled(
        num_str.clone(),
        Style::default().fg(theme.gray_dim),
    ));
    let prefix_in_selection = Line::from(Span::styled(
        num_str.clone(),
        Style::default().fg(theme.gray),
    ));
    let prefix_cursor = Line::from(Span::styled(
        num_str,
        Style::default().fg(theme.text_secondary),
    ));
    let prefix_blank = Line::from(Span::styled(blank_str, Style::default().fg(theme.gray_dim)));
    (prefix, prefix_in_selection, prefix_cursor, prefix_blank)
}

impl ListItem for SourceLine {
    fn content(&self) -> &Line<'_> {
        if self.is_markdown() {
            // Empty content triggers custom render().
            static EMPTY: std::sync::LazyLock<Line<'static>> =
                std::sync::LazyLock::new(Line::default);
            &EMPTY
        } else {
            &self.content
        }
    }

    fn prefix(&self) -> Option<Line<'_>> {
        if self.commented {
            Some(self.prefix_in_selection.clone())
        } else {
            Some(self.prefix.clone())
        }
    }

    fn prefix_in_selection(&self) -> Option<Line<'_>> {
        Some(self.prefix_in_selection.clone())
    }

    fn prefix_cursor(&self) -> Option<Line<'_>> {
        Some(self.prefix_cursor.clone())
    }

    fn stable_id(&self) -> u64 {
        self.item_id
    }

    fn search_text(&self) -> &str {
        &self.plain_text
    }

    fn copy_text(&self) -> String {
        if self.is_markdown() {
            self.plain_text.clone()
        } else {
            self.content
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        if !self.is_markdown() || width == 0 {
            return 1;
        }
        let prefix_w = self
            .prefix()
            .map(|p| crate::views::list_pane::line_display_width(&p))
            .unwrap_or(0);
        let text_w = (width as usize).saturating_sub(prefix_w).max(1);
        let mut total: u16 = 0;
        for line in &self.rendered_lines {
            let wrapped = word_wrap_line(line, text_w);
            total += (wrapped.len() as u16).max(1);
        }
        total.max(1)
    }

    fn goto_line_number(&self) -> Option<usize> {
        Some(self.line_number)
    }

    fn render(&self, area: Rect, buf: &mut Buffer, _selected: bool, _focused: bool) {
        if !self.is_markdown() || area.height == 0 || area.width == 0 {
            return;
        }
        let theme = Theme::current();
        let prefix_w = self
            .prefix()
            .map(|p| crate::views::list_pane::line_display_width(&p))
            .unwrap_or(0) as u16;
        let text_x = area.x + prefix_w;
        let text_w = area.width.saturating_sub(prefix_w);

        // Apply commented background tint to the full item area.
        if self.commented {
            buf.set_style(area, Style::default().bg(theme.bg_visual));
        }

        // Render prefix on the first visual line (brighter when commented).
        let pfx = if self.commented {
            &self.prefix_in_selection
        } else {
            &self.prefix
        };
        buf.set_line(area.x, area.y, pfx, prefix_w);

        let mut y = area.y;
        let mut is_first_visual = true;
        for (i, line) in self.rendered_lines.iter().enumerate() {
            let bg = self.rendered_bgs.get(i).copied().flatten();
            let wrapped = word_wrap_line(line, text_w as usize);
            let visual_lines = if wrapped.is_empty() {
                vec![Line::default()]
            } else {
                wrapped
            };
            for wline in &visual_lines {
                if y >= area.y + area.height {
                    break;
                }
                if let Some(bg) = bg {
                    let bg_rect = Rect {
                        x: text_x,
                        y,
                        width: text_w,
                        height: 1,
                    };
                    buf.set_style(bg_rect, Style::default().bg(bg));
                }
                if !is_first_visual {
                    buf.set_line(area.x, y, &self.prefix_blank, prefix_w);
                }
                buf.set_line(text_x, y, wline, text_w);
                y += 1;
                is_first_visual = false;
            }
        }
    }
}

// ── Comment lines ─────────────────────────────────────────────────────

/// An inline review comment displayed between source lines.
pub struct CommentLine {
    pub comment_id: u64,
    item_id: u64,
    line_label: String,
    text: String,
    prefix: Line<'static>,
}

impl CommentLine {
    pub fn new(
        comment_id: u64,
        item_id: u64,
        line_range: &std::ops::Range<usize>,
        text: String,
        max_digits: usize,
    ) -> Self {
        let theme = Theme::current();
        let pad = " ".repeat(max_digits);
        let prefix = Line::from(Span::styled(
            format!("{pad} "),
            Style::default().fg(theme.accent_plan),
        ));
        let line_label = if line_range.len() == 1 {
            format!("L{}", line_range.start)
        } else {
            format!("L{}-{}", line_range.start, line_range.end - 1)
        };
        Self {
            comment_id,
            item_id,
            line_label,
            text,
            prefix,
        }
    }
}

impl ListItem for CommentLine {
    fn content(&self) -> &Line<'_> {
        static EMPTY: std::sync::LazyLock<Line<'static>> = std::sync::LazyLock::new(Line::default);
        &EMPTY
    }

    fn prefix(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn prefix_in_selection(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn prefix_cursor(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn stable_id(&self) -> u64 {
        self.item_id
    }

    fn search_text(&self) -> &str {
        &self.text
    }

    fn copy_text(&self) -> String {
        self.text.clone()
    }

    fn desired_height(&self, width: u16) -> u16 {
        if width == 0 {
            return 1;
        }
        let prefix_w =
            crate::views::list_pane::line_display_width(&self.prefix().unwrap_or_default());
        let text_w = (width as usize).saturating_sub(prefix_w).max(1);
        self.text
            .split('\n')
            .enumerate()
            .map(|(i, text_line)| {
                let display = if i == 0 {
                    format!(
                        "{} {} {}",
                        crate::glyphs::filled_dot(),
                        self.line_label,
                        text_line
                    )
                } else {
                    format!("  {text_line}")
                };
                word_wrap_line(&Line::from(display), text_w).len() as u16
            })
            .sum::<u16>()
            .max(1)
    }

    fn render(&self, area: Rect, buf: &mut Buffer, _selected: bool, _focused: bool) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let theme = Theme::current();
        let prefix_w =
            crate::views::list_pane::line_display_width(&self.prefix().unwrap_or_default()) as u16;
        let text_x = area.x + prefix_w;
        let text_w = area.width.saturating_sub(prefix_w);

        buf.set_line(area.x, area.y, &self.prefix, prefix_w);

        let mut y = area.y;
        for (i, text_line) in self.text.split('\n').enumerate() {
            if y >= area.y + area.height {
                break;
            }
            let line = if i == 0 {
                let bullet = Span::styled(
                    format!("{} ", crate::glyphs::filled_dot()),
                    Style::default().fg(theme.accent_plan),
                );
                let label = Span::styled(
                    format!("{} ", self.line_label),
                    Style::default().fg(theme.gray),
                );
                let body = Span::styled(
                    text_line.to_owned(),
                    Style::default().fg(theme.text_primary),
                );
                Line::from(vec![bullet, label, body])
            } else {
                let indent = Span::styled("  ", Style::default().fg(theme.accent_plan));
                let body = Span::styled(
                    text_line.to_owned(),
                    Style::default().fg(theme.text_primary),
                );
                Line::from(vec![indent, body])
            };
            let wrapped = word_wrap_line(&line, text_w as usize);
            for wline in &wrapped {
                if y >= area.y + area.height {
                    break;
                }
                buf.set_line(text_x, y, wline, text_w);
                y += 1;
            }
        }
    }
}

// ── Mermaid affordance row ────────────────────────────────────────────

/// Blank reserved row under a Mermaid diagram; buttons are painted by the
/// draw loop (same pattern as scrollback).
pub struct MermaidAffordanceLine {
    item_id: u64,
    /// Fence body — data for Open / Copy path / Copy source.
    pub source: String,
    prefix: Line<'static>,
}

impl MermaidAffordanceLine {
    fn new(item_id: u64, source: String, max_digits: usize) -> Self {
        let prefix = Line::from(Span::styled(
            " ".repeat(max_digits + 1),
            Style::default().fg(Theme::current().gray_dim),
        ));
        Self {
            item_id,
            source,
            prefix,
        }
    }

    fn prefix_width(&self) -> u16 {
        crate::views::list_pane::line_display_width(&self.prefix) as u16
    }
}

impl ListItem for MermaidAffordanceLine {
    fn content(&self) -> &Line<'_> {
        static EMPTY: std::sync::LazyLock<Line<'static>> = std::sync::LazyLock::new(Line::default);
        &EMPTY
    }

    fn prefix(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn prefix_in_selection(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn prefix_cursor(&self) -> Option<Line<'_>> {
        Some(self.prefix.clone())
    }

    fn stable_id(&self) -> u64 {
        self.item_id
    }

    fn is_selectable(&self) -> bool {
        false
    }

    fn search_text(&self) -> &str {
        ""
    }

    fn copy_text(&self) -> String {
        String::new()
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }

    fn render(&self, area: Rect, buf: &mut Buffer, _selected: bool, _focused: bool) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        // Blank prefix only — write via cell_mut so out-of-bounds coords
        // cannot panic (Buffer::set_line indexes and panics on OOB).
        let prefix_w = self.prefix_width().min(area.width);
        let style = self
            .prefix
            .spans
            .first()
            .map(|s| s.style)
            .unwrap_or_default();
        for dx in 0..prefix_w {
            let Some(cell) = buf.cell_mut((area.x.saturating_add(dx), area.y)) else {
                break;
            };
            cell.set_char(' ');
            cell.set_style(style);
        }
    }
}

// ── Plan viewer item ──────────────────────────────────────────────────

/// Source line, review comment, or Mermaid affordance row.
pub enum PlanViewerItem {
    Source(Box<SourceLine>),
    Comment(CommentLine),
    MermaidAffordance(MermaidAffordanceLine),
}

impl PlanViewerItem {
    /// The 1-based source line number, if this is a source line.
    pub fn line_number(&self) -> Option<usize> {
        match self {
            Self::Source(s) => Some(s.line_number),
            Self::Comment(_) | Self::MermaidAffordance(_) => None,
        }
    }

    /// The comment ID, if this is a comment item.
    pub fn comment_id(&self) -> Option<u64> {
        match self {
            Self::Source(_) | Self::MermaidAffordance(_) => None,
            Self::Comment(c) => Some(c.comment_id),
        }
    }
}

impl ListItem for PlanViewerItem {
    fn content(&self) -> &Line<'_> {
        match self {
            Self::Source(s) => s.content(),
            Self::Comment(c) => c.content(),
            Self::MermaidAffordance(m) => m.content(),
        }
    }

    fn prefix(&self) -> Option<Line<'_>> {
        match self {
            Self::Source(s) => s.prefix(),
            Self::Comment(c) => c.prefix(),
            Self::MermaidAffordance(m) => m.prefix(),
        }
    }

    fn prefix_in_selection(&self) -> Option<Line<'_>> {
        match self {
            Self::Source(s) => s.prefix_in_selection(),
            Self::Comment(c) => c.prefix_in_selection(),
            Self::MermaidAffordance(m) => m.prefix_in_selection(),
        }
    }

    fn prefix_cursor(&self) -> Option<Line<'_>> {
        match self {
            Self::Source(s) => s.prefix_cursor(),
            Self::Comment(c) => c.prefix_cursor(),
            Self::MermaidAffordance(m) => m.prefix_cursor(),
        }
    }

    fn stable_id(&self) -> u64 {
        match self {
            Self::Source(s) => s.stable_id(),
            Self::Comment(c) => c.stable_id(),
            Self::MermaidAffordance(m) => m.stable_id(),
        }
    }

    fn is_selectable(&self) -> bool {
        match self {
            Self::Source(_) | Self::Comment(_) => true,
            Self::MermaidAffordance(m) => m.is_selectable(),
        }
    }

    fn search_text(&self) -> &str {
        match self {
            Self::Source(s) => s.search_text(),
            Self::Comment(c) => c.search_text(),
            Self::MermaidAffordance(m) => m.search_text(),
        }
    }

    fn copy_text(&self) -> String {
        match self {
            Self::Source(s) => s.copy_text(),
            Self::Comment(c) => c.copy_text(),
            Self::MermaidAffordance(m) => m.copy_text(),
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        match self {
            Self::Source(s) => s.desired_height(width),
            Self::Comment(c) => c.desired_height(width),
            Self::MermaidAffordance(m) => m.desired_height(width),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer, selected: bool, focused: bool) {
        match self {
            Self::Source(s) => s.render(area, buf, selected, focused),
            Self::Comment(c) => c.render(area, buf, selected, focused),
            Self::MermaidAffordance(m) => m.render(area, buf, selected, focused),
        }
    }

    fn goto_line_number(&self) -> Option<usize> {
        match self {
            Self::Source(s) => Some(s.line_number),
            Self::Comment(_) | Self::MermaidAffordance(_) => None,
        }
    }
}

// ── Viewer state ────────────────────────────────────────────────────────

/// What kind of content the line viewer is showing.
///
/// Replaces string-based type sniffing (`title_override == Some("plan.md")`)
/// with a typed enum so that plan-specific behavior (commenting, approval,
/// double-click, shortcuts) can be dispatched via `match` rather than
/// string comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineViewerKind {
    /// Normal file preview opened from an `@file` reference.
    #[default]
    FilePreview,
    /// Plan document preview (plan.md) — supports commenting, approval
    /// buttons, send-feedback, and double-click-to-comment.
    PlanPreview,
}

#[derive(Default)]
pub struct PlanViewerExtras {
    pub send_button_area: Option<Rect>,
    pub send_hovered: bool,
    pub show_action_buttons: bool,
    pub feedback_active: bool,
    pub approve_button_area: Option<Rect>,
    pub approve_hovered: bool,
    pub comment_button_area: Option<Rect>,
    pub comment_hovered: bool,
    pub abandon_button_area: Option<Rect>,
    pub abandon_hovered: bool,
    pub copy_button_area: Option<Rect>,
    pub copy_hovered: bool,
    pub last_click_at: Option<std::time::Instant>,
    pub gutter_drag_start: Option<usize>,
    pub gutter_drag_end: Option<usize>,
    pub gutter_hovered_line: Option<usize>,
    pub active_commenting_range: Option<std::ops::Range<usize>>,
}

/// Double-click detection threshold in milliseconds.
pub const DOUBLE_CLICK_MS: u128 = 400;

/// State for the line viewer popup.
pub struct LineViewerState {
    /// What kind of content is being viewed (file vs plan).
    pub kind: LineViewerKind,
    /// The file being viewed.
    pub path: PathBuf,
    /// Viewer items (source lines interleaved with comment annotations).
    pub lines: Vec<PlanViewerItem>,
    /// Raw source lines kept for rebuilding items when comments change.
    source_lines: Vec<SourceLine>,
    /// ListPane state for navigation + visual selection.
    pub list_state: ListPaneState,
    /// The element ID being modified (if editing an existing element).
    pub element_id: Option<ElementId>,
    /// Whether we're inside an undo group (to close/cancel on exit).
    pub in_undo_group: bool,
    /// Cached inner popup area from last render (for mouse hit-testing).
    /// Excludes the divider + footer rows in plan modes, so it matches
    /// the area the ListPane was rendered into — used for routing list
    /// events to `ListPaneState::handle_mouse_event`.
    pub last_popup_area: Option<Rect>,
    /// Cached full modal area (inside the border, including the footer)
    /// from the last render. Used by the click-outside-modal check so
    /// clicks landing on the divider or empty space between footer
    /// buttons don't accidentally close the modal.
    pub last_modal_area: Option<Rect>,
    /// Cached close button rect from last render (for mouse hit-testing).
    pub close_button_area: Option<Rect>,
    /// Whether the close button is hovered (for highlight).
    pub close_hovered: bool,
    /// Cached fullscreen toggle button rect from last render.
    pub fullscreen_button_area: Option<Rect>,
    /// Whether the fullscreen button is hovered.
    pub fullscreen_hovered: bool,
    /// Plan-specific state. `Some` only when `kind == PlanPreview`.
    /// Keeps plan-only fields (buttons, approval, double-click) out of
    /// the generic viewer.
    pub plan: Option<PlanViewerExtras>,
    /// Initial scroll range (0-based indices). Consumed on first prepare_layout
    /// to center the range in the viewport.
    initial_scroll_range: Option<Range<usize>>,
    /// Optional title override. When set, the viewer title bar shows this
    /// instead of the (potentially long) file path. Used for plan previews.
    pub title_override: Option<String>,
    /// Raw markdown content for rebuilding when the display width changes.
    /// `None` for non-markdown viewers.
    markdown_content: Option<String>,
    /// The last `max_table_width` used to build markdown lines.
    /// Compared against the current content width in `prepare_layout` to
    /// trigger a rebuild when the viewer is resized.
    last_table_width: Option<usize>,
    /// Copy of comments last applied via `rebuild_with_comments`, so that
    /// a width-triggered rebuild can re-interleave them automatically.
    last_comments: Vec<crate::views::plan_approval_view::PlanComment>,
    /// `(source_lines index to follow, diagram source)` for affordance rows.
    mermaid_after: Vec<(usize, String)>,
    /// When `true`, the viewer uses the full overlay area instead of the
    /// 75% centered popup. Toggled by Ctrl+F.
    pub fullscreen: bool,
}

impl LineViewerState {
    /// Open a file and create the viewer state.
    ///
    /// Returns `None` if the file can't be read.
    pub fn open(path: &Path, element_id: Option<ElementId>) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let source_lines = build_source_lines(path, &content);
        let lines = source_lines
            .iter()
            .cloned()
            .map(|s| PlanViewerItem::Source(Box::new(s)))
            .collect();

        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: true,
            visual_select_enabled: true,
            filter_enabled: false,
            goto_line_enabled: true,
        };
        let list_state = ListPaneState::new_with_config(WrapMode::Wrap, false, config);

        Some(Self {
            kind: LineViewerKind::FilePreview,
            path: path.to_path_buf(),
            lines,
            source_lines,
            list_state,
            element_id,
            in_undo_group: false,
            last_popup_area: None,
            last_modal_area: None,
            close_button_area: None,
            close_hovered: false,
            fullscreen_button_area: None,
            fullscreen_hovered: false,
            plan: None,
            initial_scroll_range: None,
            title_override: None,
            markdown_content: None,
            last_table_width: None,
            last_comments: Vec::new(),
            mermaid_after: Vec::new(),
            fullscreen: false,
        })
    }

    /// Open a file and create the viewer with markdown rendering.
    ///
    /// Same as `open()` but renders the content as rich markdown instead of
    /// raw syntax-highlighted text. Source-line-based navigation is preserved.
    pub fn open_markdown(path: &Path, element_id: Option<ElementId>) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Self::open_markdown_content(path.to_path_buf(), content, element_id)
    }

    pub fn open_markdown_content(
        path: impl Into<PathBuf>,
        content: String,
        element_id: Option<ElementId>,
    ) -> Option<Self> {
        if content.trim().is_empty() {
            return None;
        }
        let path = path.into();

        // Defer the actual markdown render to the first `prepare_layout` call
        // which knows the display width. `rebuild_markdown_for_width` will
        // build source_lines with the correct `max_table_width`.
        let source_lines = Vec::new();
        let lines = Vec::new();

        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: true,
            visual_select_enabled: true,
            filter_enabled: false,
            goto_line_enabled: true,
        };
        let list_state = ListPaneState::new_with_config(WrapMode::Wrap, false, config);

        Some(Self {
            kind: LineViewerKind::FilePreview,
            path,
            lines,
            source_lines,
            list_state,
            element_id,
            in_undo_group: false,
            last_popup_area: None,
            last_modal_area: None,
            close_button_area: None,
            close_hovered: false,
            fullscreen_button_area: None,
            fullscreen_hovered: false,
            plan: None,
            initial_scroll_range: None,
            title_override: None,
            markdown_content: Some(content),
            last_table_width: None,
            last_comments: Vec::new(),
            mermaid_after: Vec::new(),
            fullscreen: false,
        })
    }

    /// Set initial selection and scroll to a line range (1-based).
    ///
    /// Enters visual mode with the range pre-selected and scrolls so the
    /// range is visible (centered if possible).
    pub fn set_initial_selection(&mut self, range: Range<usize>) {
        // Convert 1-based line numbers to 0-based ListPane indices.
        let start_idx = range.start.saturating_sub(1);
        let end_idx = range.end.saturating_sub(1).min(self.lines.len());

        if start_idx < self.lines.len() {
            // Select the start line.
            let start_id = self.lines[start_idx].stable_id();
            self.list_state.select_by_id(start_id);

            // Enter visual mode and extend to end line.
            if end_idx > start_idx + 1 {
                // Multi-line range: enter visual mode.
                self.list_state.enter_visual_mode(&self.lines);
                // Move selection to the end of the range.
                let end_line_idx = (end_idx - 1).min(self.lines.len() - 1);
                let end_id = self.lines[end_line_idx].stable_id();
                self.list_state.select_by_id(end_id);
            }

            // Store the range for scroll centering on first render.
            self.initial_scroll_range = Some(start_idx..end_idx);
        }
    }

    /// Rebuild markdown items when the available content width changes.
    ///
    /// Recomputes `max_table_width` from the ListPane's content width
    /// (total width minus line-number prefix), then re-renders markdown
    /// with constrained tables so box-drawing borders aren't word-wrapped.
    fn rebuild_markdown_for_width(&mut self, width: u16) {
        let Some(ref content) = self.markdown_content else {
            return;
        };

        let prefix_width = digit_count(source_line_count(content).max(1)) + 1;
        let scrollbar_width = SCROLLBAR_TOTAL_COLS as usize; // gap + track
        let content_width = (width as usize)
            .saturating_sub(prefix_width)
            .saturating_sub(scrollbar_width);

        if self.last_table_width == Some(content_width) {
            return;
        }
        self.last_table_width = Some(content_width);

        let built = build_markdown_lines(content, Some(content_width));
        self.source_lines = built.source_lines;
        self.mermaid_after = built.mermaid_after;

        if self.last_comments.is_empty() && self.mermaid_after.is_empty() {
            self.lines = self
                .source_lines
                .iter()
                .cloned()
                .map(|s| PlanViewerItem::Source(Box::new(s)))
                .collect();
        } else {
            let comments = self.last_comments.clone();
            self.interleave_comments(&comments);
        }
    }

    /// Screen rects for visible Mermaid affordance rows (for paint + hit-testing).
    pub fn diagram_affordance_placements(
        &self,
        content_area: Rect,
    ) -> Vec<DiagramAffordancePlacement> {
        if content_area.width == 0 || content_area.height == 0 {
            return Vec::new();
        }

        let scroll = self.list_state.scroll_offset();
        let layout = self.list_state.layout();
        let visible = self.list_state.visible_range();
        if visible.is_empty() {
            return Vec::new();
        }
        let first_vi = visible.start;
        let skip_first = self.list_state.first_item_skip_rows();
        let mut placements = Vec::new();

        for vi in visible {
            let pi = self.list_state.to_physical(vi);
            let Some(PlanViewerItem::MermaidAffordance(m)) = self.lines.get(pi) else {
                continue;
            };
            let item_h = layout.item_height(vi);
            let skip = if vi == first_vi { skip_first } else { 0 };
            if skip >= item_h {
                continue;
            }
            // Align with list-pane layout: first visible item may be top-clipped.
            let screen_y_offset = layout
                .virtual_y(vi)
                .saturating_sub(scroll)
                .saturating_add(skip as usize);
            if screen_y_offset >= content_area.height as usize {
                continue;
            }
            let prefix_w = m.prefix_width();
            let text_w = content_area
                .width
                .saturating_sub(prefix_w)
                .saturating_sub(SCROLLBAR_TOTAL_COLS);
            if text_w == 0 {
                continue;
            }
            placements.push(DiagramAffordancePlacement {
                screen_rect: Rect {
                    x: content_area.x.saturating_add(prefix_w),
                    y: content_area.y.saturating_add(screen_y_offset as u16),
                    width: text_w,
                    height: 1,
                },
                source: m.source.clone(),
            });
        }
        placements
    }

    #[cfg(test)]
    pub(crate) fn markdown_content_for_test(&self) -> Option<&str> {
        self.markdown_content.as_deref()
    }

    /// Returns the raw markdown content for feedback formatting.
    pub fn markdown_content_for_feedback(&self) -> Option<String> {
        self.markdown_content.clone()
    }

    /// Mutable access to plan-specific extras, initializing if needed.
    pub fn plan_mut(&mut self) -> &mut PlanViewerExtras {
        self.plan.get_or_insert_with(PlanViewerExtras::default)
    }

    /// Read-only access to plan-specific extras.
    pub fn plan_ref(&self) -> Option<&PlanViewerExtras> {
        self.plan.as_ref()
    }

    pub fn feedback_active(&self) -> bool {
        self.plan.as_ref().is_some_and(|p| p.feedback_active)
    }

    /// Whether the plan modal should render the action-button footer.
    /// True for plan-approval and casual plan preview (not plain file preview).
    pub fn show_footer(&self) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|p| p.feedback_active || p.show_action_buttons)
    }

    pub fn source_line_at_screen_row(&self, row: u16, content_area: Rect) -> Option<usize> {
        if row < content_area.y || row >= content_area.y + content_area.height {
            return None;
        }
        let ry = (row - content_area.y) as usize;
        let vy = self.list_state.scroll_offset() + ry;
        let vi = self.list_state.layout().item_at_y(vy)?;
        let pi = self.list_state.to_physical(vi);
        self.lines.get(pi)?.line_number()
    }

    /// Prepare the layout for rendering (must be called each frame).
    pub fn prepare_layout(&mut self, width: u16, height: u16) {
        self.rebuild_markdown_for_width(width);
        self.list_state.prepare_layout(&self.lines, width, height);

        // On the first render with an initial range, center the range
        // in the viewport. Consumed once so subsequent navigation is normal.
        if let Some(range) = self.initial_scroll_range.take() {
            let vp = height as usize;
            let total = self.lines.len();
            let pad = 3usize; // inner padding (lines of context above/below)

            if total <= vp {
                // Entire file fits — no scrolling needed.
            } else {
                let range_len = range.end.saturating_sub(range.start);
                let offset = if range_len + pad * 2 <= vp {
                    // Range fits with padding — center it.
                    let center = range.start + range_len / 2;
                    center.saturating_sub(vp / 2)
                } else {
                    // Range larger than viewport — put start near top with padding.
                    range.start.saturating_sub(pad)
                };
                // Clamp to valid range.
                let max_offset = total.saturating_sub(vp);
                self.list_state.set_scroll_offset(offset.min(max_offset));
            }
        }
    }

    /// Rebuild `self.lines` from source lines + comments.
    ///
    /// Comments are inserted after the last source line in their range.
    /// Item IDs for comments use a high base offset to avoid colliding
    /// with source line IDs.
    pub fn rebuild_with_comments(
        &mut self,
        comments: &[crate::views::plan_approval_view::PlanComment],
    ) {
        self.last_comments = comments.to_vec();
        self.interleave_comments(comments);
        // Item count/content changed — force the list pane to recompute
        // wrapping heights on the next render frame.
        self.list_state.invalidate_layout();
    }

    /// Interleave source lines with Mermaid affordance rows and comments
    /// without updating `last_comments`.
    ///
    /// `mermaid_after` is document-ordered; affordances sit under the
    /// diagram art, before any comments on the same source line.
    fn interleave_comments(&mut self, comments: &[crate::views::plan_approval_view::PlanComment]) {
        let max_digits = digit_count(
            self.source_lines
                .last()
                .map(|s| s.line_number)
                .unwrap_or(1)
                .max(1),
        );

        let mut sorted: Vec<_> = comments.iter().collect();
        sorted.sort_by_key(|c| c.line_range.end);

        let mut commented_lines = std::collections::HashSet::new();
        for c in comments {
            for ln in c.line_range.clone() {
                commented_lines.insert(ln);
            }
        }

        let mut items: Vec<PlanViewerItem> = Vec::new();
        let mut comment_idx = 0;
        let comment_id_base: u64 = 1_000_000;
        let mut mermaid_i = 0usize;

        for (src_idx, src) in self.source_lines.iter().enumerate() {
            let ln = src.line_number;
            let mut src = src.clone();
            src.commented = commented_lines.contains(&ln);
            items.push(PlanViewerItem::Source(Box::new(src)));

            while mermaid_i < self.mermaid_after.len() && self.mermaid_after[mermaid_i].0 == src_idx
            {
                items.push(PlanViewerItem::MermaidAffordance(
                    MermaidAffordanceLine::new(
                        MERMAID_AFFORDANCE_ID_BASE + mermaid_i as u64,
                        self.mermaid_after[mermaid_i].1.clone(),
                        max_digits,
                    ),
                ));
                mermaid_i += 1;
            }

            while comment_idx < sorted.len() && sorted[comment_idx].line_range.end == ln + 1 {
                let c = sorted[comment_idx];
                let item_id = comment_id_base + c.id;
                items.push(PlanViewerItem::Comment(CommentLine::new(
                    c.id,
                    item_id,
                    &c.line_range,
                    c.text.clone(),
                    max_digits,
                )));
                comment_idx += 1;
            }
        }

        for c in &sorted[comment_idx..] {
            let item_id = comment_id_base + c.id;
            items.push(PlanViewerItem::Comment(CommentLine::new(
                c.id,
                item_id,
                &c.line_range,
                c.text.clone(),
                max_digits,
            )));
        }

        self.lines = items;
    }

    /// Get the selected line range (1-based, inclusive).
    ///
    /// Returns a single line if no visual selection, or the visual range.
    pub fn selected_line_range(&self) -> Option<Range<usize>> {
        if let Some(range) = self.list_state.multi_range() {
            // Scan forward from start to find first source line.
            let start = (range.start..range.end).find_map(|i| self.lines.get(i)?.line_number())?;
            // Scan backward from end to find last source line.
            let end = (range.start..range.end)
                .rev()
                .find_map(|i| self.lines.get(i)?.line_number())?;
            Some(start..end + 1)
        } else {
            let idx = self.list_state.selected_index()?;
            let ln = self.lines.get(idx)?.line_number()?;
            Some(ln..ln + 1)
        }
    }

    /// Format the line range as a suffix string (e.g., `:10` or `:10-12`).
    pub fn line_range_suffix(&self) -> Option<String> {
        let range = self.selected_line_range()?;
        if range.len() == 1 {
            Some(format!(":{}", range.start))
        } else {
            Some(format!(":{}-{}", range.start, range.end - 1))
        }
    }

    /// ListPane style matching the pager's visual language.
    pub fn list_pane_style() -> ListPaneStyle {
        let theme = Theme::current();
        ListPaneStyle {
            input_bar_bg: theme.bg_dark,
            input_bar_prompt_fg: theme.command,
            scrollbar_bg: theme.bg_dark,
            scrollbar_fg: theme.gray_dim,
            uniform_visual_bg: true,
            ..ListPaneStyle::default()
        }
    }
}

// ── Syntax highlighting ─────────────────────────────────────────────────

/// Build syntax-highlighted source lines from file content.
fn build_source_lines(path: &Path, content: &str) -> Vec<SourceLine> {
    let syntect = get_syntect();
    let mut highlighter = syntect.highlight_lines_by_file_path(path);

    // Split preserving all lines including trailing empty ones.
    // `split('\n')` keeps trailing empty strings unlike `.lines()`.
    let raw_lines: Vec<&str> = content.split('\n').collect();
    // If the file ends with a newline, remove the trailing empty split artifact.
    let line_count = if content.ends_with('\n') && raw_lines.last() == Some(&"") {
        raw_lines.len() - 1
    } else {
        raw_lines.len()
    };
    let max_digits = digit_count(line_count.max(1));

    raw_lines
        .iter()
        .take(line_count)
        .enumerate()
        .map(|(i, text)| {
            let line_number = i + 1;
            // Feed every line, blank ones included, through the highlighter so
            // its parse state stays in sync. Skipping blanks corrupts constructs
            // that span multiple lines (block comments, multi-line strings).
            let styled_line = match highlighter.as_mut() {
                Some(hl) => highlight_to_ratatui_line(hl, text, &syntect.syntax_set),
                None if text.is_empty() => Line::from(" ".to_owned()),
                None => Line::from((*text).to_owned()),
            };
            SourceLine::new(line_number, styled_line, (*text).to_owned(), max_digits)
        })
        .collect()
}

/// Count source lines in content, matching `str::split('\n')` semantics
/// with trailing-newline handling.
fn source_line_count(content: &str) -> usize {
    let count = content.split('\n').count();
    if content.ends_with('\n') {
        count - 1
    } else {
        count
    }
}

struct BuiltMarkdownLines {
    source_lines: Vec<SourceLine>,
    /// Document-ordered `(source_lines index to follow, diagram source)`.
    mermaid_after: Vec<(usize, String)>,
}

/// Build markdown-rendered source lines from file content.
///
/// Uses `MarkdownContent` to render the full document, then groups rendered
/// lines by source line using `line_source_map`. Each source line becomes
/// one `SourceLine` item that may span multiple visual lines (e.g., a table
/// block renders as border + header + separator + data + border).
///
/// With `render_mermaid` auto/on, also anchors affordance rows under each
/// closed mermaid fence.
fn build_markdown_lines(content: &str, max_table_width: Option<usize>) -> BuiltMarkdownLines {
    let md = MarkdownContent::new_source_faithful(content, max_table_width);
    let pre_wrap = md.pre_wrap_lines();
    let source_map = md.line_source_map();
    let mermaid = md.mermaid_content();
    let mermaid_ranges = md.mermaid_block_ranges();

    // Background colors come from each line's style (set by the renderer
    // for code blocks etc.). pre_wrap_lines() returns owned Lines that
    // carry their style including bg.
    let line_bgs: Vec<Option<Color>> = pre_wrap.iter().map(|line| line.style.bg).collect();

    // Split source text into raw lines for plain_text / search.
    let raw_lines: Vec<&str> = content.split('\n').collect();
    let slc = source_line_count(content);
    let max_digits = digit_count(slc.max(1));

    // Group by source line; track which group each pre-wrap line lands in.
    let mut groups: Vec<(usize, Vec<Line<'static>>, Vec<Option<Color>>)> = Vec::new();
    let mut prewrap_to_group: Vec<usize> = Vec::with_capacity(pre_wrap.len());
    for (rendered_idx, rendered_line) in pre_wrap.into_iter().enumerate() {
        let src_line = source_map.get(rendered_idx).copied().unwrap_or(0);
        let bg = line_bgs.get(rendered_idx).copied().flatten();
        if let Some(last) = groups.last_mut()
            && last.0 == src_line
        {
            last.1.push(rendered_line);
            last.2.push(bg);
            prewrap_to_group.push(groups.len() - 1);
            continue;
        }
        groups.push((src_line, vec![rendered_line], vec![bg]));
        prewrap_to_group.push(groups.len() - 1);
    }

    // group index → source_lines index after blank-line injection.
    let mut group_to_source_idx: Vec<usize> = Vec::with_capacity(groups.len());
    let mut source_lines = Vec::new();
    let mut next_item_id = 0u64;
    let mut next_blank_src = 0usize;

    for (src_line_0based, rendered_lines, rendered_bgs) in groups {
        for blank_src in next_blank_src..src_line_0based.min(slc) {
            if raw_lines
                .get(blank_src)
                .is_some_and(|line| line.trim().is_empty())
            {
                source_lines.push(SourceLine::new_markdown(
                    next_item_id,
                    blank_src + 1,
                    vec![Line::default()],
                    vec![None],
                    raw_lines.get(blank_src).unwrap_or(&"").to_string(),
                    max_digits,
                ));
                next_item_id += 1;
            }
        }

        group_to_source_idx.push(source_lines.len());
        source_lines.push(SourceLine::new_markdown(
            next_item_id,
            src_line_0based + 1,
            rendered_lines,
            rendered_bgs,
            raw_lines.get(src_line_0based).unwrap_or(&"").to_string(),
            max_digits,
        ));
        next_item_id += 1;
        next_blank_src = next_blank_src.max(src_line_0based.saturating_add(1));
    }

    for blank_src in next_blank_src..slc {
        if raw_lines
            .get(blank_src)
            .is_some_and(|line| line.trim().is_empty())
        {
            source_lines.push(SourceLine::new_markdown(
                next_item_id,
                blank_src + 1,
                vec![Line::default()],
                vec![None],
                raw_lines.get(blank_src).unwrap_or(&"").to_string(),
                max_digits,
            ));
            next_item_id += 1;
        }
    }

    let show_affordances = mermaid_display(crate::appearance::cache::load_render_mermaid())
        == MermaidDisplay::Affordances;
    let mut mermaid_after = Vec::new();
    if show_affordances {
        for (i, range) in mermaid_ranges.iter().enumerate() {
            if range.is_empty() {
                continue;
            }
            let Some(&group_idx) = prewrap_to_group.get(range.end - 1) else {
                continue;
            };
            let Some(&src_idx) = group_to_source_idx.get(group_idx) else {
                continue;
            };
            let Some(source) = mermaid.source(i) else {
                continue;
            };
            mermaid_after.push((src_idx, source.to_owned()));
        }
    }

    BuiltMarkdownLines {
        source_lines,
        mermaid_after,
    }
}

/// Convert syntect highlighting output to a ratatui Line.
fn highlight_to_ratatui_line(
    hl: &mut HighlightLines<'_>,
    text: &str,
    syntax_set: &syntect::parsing::SyntaxSet,
) -> Line<'static> {
    // syntect needs the trailing newline to recognize line-spanning constructs.
    // Feed it, then strip the newline back out of the rendered spans.
    let with_newline = format!("{text}\n");
    let highlighted = match hl.highlight_line(&with_newline, syntax_set) {
        Ok(h) => h,
        Err(_) if text.is_empty() => return Line::from(" ".to_owned()),
        Err(_) => return Line::from(text.to_owned()),
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (style, segment) in highlighted {
        let mut piece = segment.to_owned();
        while piece.ends_with('\n') || piece.ends_with('\r') {
            piece.pop();
        }
        if piece.is_empty() {
            continue;
        }
        // Shared path: polarity-safe under the terminal-native lock, else
        // normal theme quantize (see echo_agent_pager_render::syntax).
        let fg = crate::syntax::syntect_rgb_to_fg(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        );
        spans.push(Span::styled(piece, Style::default().fg(fg)));
    }

    if spans.is_empty() {
        return Line::from(" ".to_owned());
    }
    Line::from(spans)
}

/// Count digits in a number (for line number padding).
fn digit_count(n: usize) -> usize {
    if n == 0 {
        1
    } else {
        ((n as f64).log10().floor() as usize) + 1
    }
}

// ── Rendering helpers ───────────────────────────────────────────────────

/// Build a single review-footer shortcut button styled to match the
/// shortcut hints in `modal_window::render_modal_shortcuts`:
/// bold key in the primary text color + dim label, with a
/// hover-highlighted background.
fn build_shortcut_button<'a>(
    key: char,
    rest: &str,
    hovered: bool,
    theme: &crate::theme::Theme,
) -> Vec<Span<'a>> {
    let bg = if hovered {
        theme.bg_highlight
    } else {
        theme.bg_base
    };
    let key_style = Style::default()
        .fg(theme.text_primary)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(theme.gray).bg(bg);
    vec![
        Span::styled(key.to_string(), key_style),
        Span::styled(format!(" {rest}"), label_style),
    ]
}

/// Render the line viewer popup.
///
/// In normal mode, draws a 75% centered panel with dimmed background
/// (modifiers reset). In fullscreen mode (`viewer.fullscreen`), fills
/// the entire overlay area without dimming. Renders the ListPane
/// inside the panel with syntax-highlighted lines.
pub fn render_line_viewer(
    buf: &mut Buffer,
    full_area: Rect,
    viewer: &mut LineViewerState,
    cwd: &Path,
    theme: &Theme,
    comment_count: usize,
) {
    // Compute popup area. In enlarge (fullscreen) mode the popup
    // nearly fills the overlay, but leaves 1 row of top padding and
    // 2 cols of side padding so it doesn't crowd the screen edges
    // (the caller already excludes the prompt + turn_status from
    // `full_area`). In normal mode it sits in a 75% centered popup.
    let (popup_area, should_dim) = if viewer.fullscreen {
        const TOP_PAD: u16 = 1;
        const SIDE_PAD: u16 = 2;
        let pad_w = SIDE_PAD.saturating_mul(2);
        let popup_x = full_area.x + SIDE_PAD.min(full_area.width);
        let popup_y = full_area.y + TOP_PAD.min(full_area.height);
        let popup_w = full_area.width.saturating_sub(pad_w);
        let popup_h = full_area.height.saturating_sub(TOP_PAD);
        (Rect::new(popup_x, popup_y, popup_w, popup_h), false)
    } else {
        let popup_width = (full_area.width as f32 * 0.75) as u16;
        let popup_height = (full_area.height as f32 * 0.75) as u16;
        let popup_x = full_area.x + (full_area.width.saturating_sub(popup_width)) / 2;
        let popup_y = full_area.y + (full_area.height.saturating_sub(popup_height)) / 2;
        (Rect::new(popup_x, popup_y, popup_width, popup_height), true)
    };

    // Plan modes (both review and casual) reserve 2 extra rows inside
    // the frame for the divider + action-button footer, so they need
    // a slightly taller minimum than ordinary file previews.
    let min_height: u16 = if viewer.show_footer() { 7 } else { 5 };
    if popup_area.width < 10 || popup_area.height < min_height {
        viewer.last_popup_area = None;
        viewer.last_modal_area = None;
        return;
    }

    // 1. Dim the entire screen behind the popup (skip when fullscreen).
    if should_dim {
        dim_area(buf, full_area, theme.bg_base, 0.5);
    }

    // 2. Clear the popup area.
    ratatui::widgets::Clear.render(popup_area, buf);
    buf.set_style(
        popup_area,
        Style::default().fg(theme.text_primary).bg(theme.bg_base),
    );

    // 3. Draw border.
    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(theme.gray_dim))
        .style(Style::default().bg(theme.bg_base));
    let inner = border.inner(popup_area);
    border.render(popup_area, buf);

    // Plan modes reserve 2 rows at the bottom of `inner` for the
    // divider + action-button row (rendered in step 8 below). Compute
    // the actual content_area now so `prepare_layout` sees the true
    // viewport height — passing the larger `inner.height` would make
    // `ListPaneState`'s auto-scroll/paging math off by `footer_rows`
    // (the selection can hide behind the footer; Ctrl-D/Ctrl-U jump
    // too far; initial-scroll-to-range centering is mis-sized).
    let footer_rows: u16 = if viewer.show_footer() { 2 } else { 0 };
    let content_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(footer_rows),
    };

    // 4. Prepare layout (resolves selection index for title + rendering).
    viewer.prepare_layout(content_area.width, content_area.height);

    // 5. Title bar: styled file path + line range.
    //    Only show line range when visual selection is active.
    //    When title_override is set (e.g. plan preview), use that instead
    //    of the full file path to avoid overflow.
    if inner.width > 4 {
        let rel_path = viewer.path.strip_prefix(cwd).unwrap_or(&viewer.path);
        let rel_path_str = viewer
            .title_override
            .as_deref()
            .unwrap_or_else(|| rel_path.to_str().unwrap_or(""))
            .to_string();
        let line_range = if viewer.list_state.visual_mode {
            viewer.line_range_suffix().map(|s| {
                // Strip leading ':' — styled_file_ref adds its own.
                s.strip_prefix(':').unwrap_or(&s).to_owned()
            })
        } else {
            None
        };

        // Build styled title with shared helper.
        let mut title = super::styled_file_ref(
            &rel_path_str,
            line_range.as_deref(),
            theme,
            false, // no @ prefix in viewer title
        );
        // Add bg to all spans (title sits on the border).
        for span in &mut title.spans {
            span.style = span.style.bg(theme.bg_base);
        }

        // Wrap with `─ ... ─` decorations to match other modals
        // (see modal_window.rs:341-346) and left-align flush with the
        // top-left corner.
        let deco = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
        title.spans.insert(0, Span::styled("\u{2500} ", deco));
        title.spans.push(Span::styled(" \u{2500}", deco));

        let title_width = title.width() as u16;
        let title_x = popup_area.x + 1;
        let max_title_width = popup_area.width.saturating_sub(2);
        buf.set_line(
            title_x,
            popup_area.y,
            &title,
            title_width.min(max_title_width),
        );
    }

    // 6. Action buttons on the top border, right-aligned.
    //    Layout: ... [↗][✗]  (rightmost buttons first; the two abut
    //    flush — see the spacing notes on the close/fullscreen labels
    //    below).
    //    The close [✗] is omitted in plan-review (feedback) mode because
    //    the modal is not user-closeable in that state — clicking it
    //    would be a no-op (see the close-button branch of
    //    `handle_line_viewer_mouse` in agent_view.rs).
    let mut right_edge = popup_area.x + popup_area.width - 1;

    if !viewer.feedback_active() {
        let close_text = crate::glyphs::ballot_x(); // ✗ (ASCII on legacy ConHost)
        // Label is `[✗] ` (trailing space, no leading space). The
        // fullscreen button's label has no trailing space when the
        // close is visible, so the two buttons abut flush as `[↗][✗]`,
        // tucked under the top-right corner with one space inside the
        // frame on each side: ` [↗][✗] `.
        let close_w: u16 = 4; // "[✗] "
        if popup_area.width > close_w + 2 {
            let close_x = right_edge - close_w;
            let close_style = if viewer.close_hovered {
                Style::default()
                    .fg(theme.text_primary)
                    .bg(theme.bg_base)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.gray).bg(theme.bg_base)
            };
            let close_span = Span::styled(format!("[{close_text}] "), close_style);
            buf.set_span(close_x, popup_area.y, &close_span, close_w);
            viewer.close_button_area = Some(Rect::new(close_x, popup_area.y, close_w, 1));
            right_edge = close_x;
        } else {
            viewer.close_button_area = None;
        }
    } else {
        viewer.close_button_area = None;
    }

    // Fullscreen toggle button. The icon stays constant regardless of
    // current state — the button is a toggle, not a status indicator.
    //
    // Spacing: when the close button is rendered (casual mode) the
    // fullscreen drops its trailing space so the two sit flush as
    // `[↗][✗]`. When the close is hidden (plan-review mode) the
    // fullscreen keeps its trailing space so it doesn't crowd the
    // corner `╮`.
    let fs_icon = crate::glyphs::enlarge(); // ↗ (ASCII on legacy ConHost)
    let close_visible = viewer.close_button_area.is_some();
    let (fs_label, fs_w): (String, u16) = if close_visible {
        (format!(" [{fs_icon}]"), 4)
    } else {
        (format!(" [{fs_icon}] "), 5)
    };
    if right_edge > popup_area.x + fs_w + 2 {
        let fs_x = right_edge - fs_w;
        let fs_style = if viewer.fullscreen_hovered {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray).bg(theme.bg_base)
        };
        let fs_span = Span::styled(fs_label, fs_style);
        buf.set_span(fs_x, popup_area.y, &fs_span, fs_w);
        viewer.fullscreen_button_area = Some(Rect::new(fs_x, popup_area.y, fs_w, 1));
    } else {
        viewer.fullscreen_button_area = None;
    }

    // The legacy top-border "send" button is gone — both plan-approval
    // and casual modes now render the send action in the modal footer
    // alongside the other shortcut buttons. Clear stale hit-rects so
    // mouse handlers don't act on positions from a previous render.
    if let Some(plan) = viewer.plan.as_mut() {
        plan.send_button_area = None;
        plan.approve_button_area = None;
        plan.abandon_button_area = None;
    }

    // 7. Render ListPane.
    //    Plan modes (review + casual) reserve 2 rows at the bottom of
    //    `inner` for a horizontal divider plus the action-button row.
    //    The divider sits at `inner.bottom() - 2` and the buttons at
    //    `inner.bottom() - 1`, both inside the modal frame.
    //
    //    `footer_rows` / `content_area` were computed above (just
    //    after `inner`) so `prepare_layout` could see the true
    //    viewport height. Reuse them here.
    let style = LineViewerState::list_pane_style();

    let pane = ListPane::new(&viewer.lines).focused(true).style(style);
    StatefulWidget::render(pane, content_area, buf, &mut viewer.list_state);

    // Cache the list-rendered area (for ListPane mouse dispatch) and
    // the full modal area inside the border (for the click-outside
    // check that decides whether to close the modal).
    viewer.last_popup_area = Some(content_area);
    viewer.last_modal_area = Some(inner);

    // 7b. Line range highlight — active drag or commenting range.
    if let Some(plan) = viewer.plan_ref() {
        let highlight_range =
            if let (Some(start), Some(end)) = (plan.gutter_drag_start, plan.gutter_drag_end) {
                if start != end {
                    Some((start.min(end), start.max(end)))
                } else {
                    None
                }
            } else {
                plan.active_commenting_range
                    .as_ref()
                    .map(|r| (r.start, r.end.saturating_sub(1)))
            };
        if let Some((lo, hi)) = highlight_range {
            let blend_bg =
                crate::render::color::blend_color(theme.bg_base, theme.accent_plan, 0.15)
                    .unwrap_or(theme.accent_plan);
            // Stop the highlight one column before the scrollbar so
            // the gap + track stay readable instead of being tinted
            // by the comment-range overlay.
            let highlight_width = content_area.width.saturating_sub(SCROLLBAR_TOTAL_COLS);
            for row in content_area.y..content_area.y + content_area.height {
                if let Some(ln) = viewer.source_line_at_screen_row(row, content_area)
                    && ln >= lo
                    && ln <= hi
                {
                    let row_rect = Rect::new(content_area.x, row, highlight_width, 1);
                    buf.set_style(row_rect, Style::default().bg(blend_bg));
                }
            }
        }
    }

    // 8. Action buttons inside the modal footer (centered), for both
    //    plan-approval and casual plan-preview modes. A full-width `─`
    //    divider separates the button row from the content above
    //    (matches modal_window.rs's tab divider style).
    //
    //    Buttons use the same `key bold + label dim` treatment as
    //    `render_modal_shortcuts`, sit in a single row separated by
    //    `  |  `, centered within the modal frame.
    //    Casual preview omits `q` (close via the X button).
    if viewer.show_footer() && inner.height >= 2 {
        let div_y = inner.y + inner.height - 2;
        let div_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
        let line: String = std::iter::repeat_n('\u{2500}', inner.width as usize).collect();
        buf.set_string(inner.x, div_y, &line, div_style);

        let bottom_y = inner.y + inner.height - 1;

        let abandon_hovered = viewer.plan_ref().is_some_and(|p| p.abandon_hovered);
        let comment_hovered = viewer.plan_ref().is_some_and(|p| p.comment_hovered);
        let approve_hovered = viewer.plan_ref().is_some_and(|p| p.approve_hovered);
        let copy_hovered = viewer.plan_ref().is_some_and(|p| p.copy_hovered);
        let is_approval = viewer.feedback_active();

        let comment_spans = build_shortcut_button('c', "comment", comment_hovered, theme);
        let comment_w: u16 = comment_spans.iter().map(|s| s.width() as u16).sum();

        let copy_spans = build_shortcut_button('y', "copy plan", copy_hovered, theme);
        let copy_w: u16 = copy_spans.iter().map(|s| s.width() as u16).sum();

        // In approval mode, always show `a approve`. When there are
        // pending review comments, also show `s revise` (request changes).
        // In approval mode, show `a approve` (or `a approve w/ comments`
        // when inline comments are pending). In casual mode, show `s send`
        // only when comments exist.
        let (_action_label, action_w, action_spans): (&str, u16, Option<Vec<Span>>) = if is_approval
        {
            let label = if comment_count > 0 {
                "approve w/ comments"
            } else {
                "approve"
            };
            let spans = build_shortcut_button('a', label, approve_hovered, theme);
            let w: u16 = spans.iter().map(|s| s.width() as u16).sum();
            (label, w, Some(spans))
        } else if comment_count > 0 {
            let spans = build_shortcut_button('s', "send", approve_hovered, theme);
            let w: u16 = spans.iter().map(|s| s.width() as u16).sum();
            ("send", w, Some(spans))
        } else {
            ("", 0, None)
        };

        // `s revise` button — always visible in approval mode so the
        // user can request changes (switches to prompt for revision notes).
        let (revise_w, revise_spans): (u16, Option<Vec<Span>>) = if is_approval {
            let send_hovered = viewer.plan_ref().is_some_and(|p| p.send_hovered);
            let spans = build_shortcut_button('s', "request changes", send_hovered, theme);
            let w: u16 = spans.iter().map(|s| s.width() as u16).sum();
            (w, Some(spans))
        } else {
            (0, None)
        };

        // Quit button only renders in approval mode (casual closes via X).
        let quit_spans = if is_approval {
            let s = build_shortcut_button('q', "quit plan", abandon_hovered, theme);
            let w: u16 = s.iter().map(|s| s.width() as u16).sum();
            Some((s, w))
        } else {
            None
        };

        // Pending-comment badge rendered after the `c comment` button
        // as ` N ●` in `accent_plan`. Shown whenever comments exist.
        use unicode_width::UnicodeWidthStr;
        let badge_text: String = if comment_count > 0 {
            format!(" {comment_count} {}", crate::glyphs::filled_dot())
        } else {
            String::new()
        };
        let badge_w: u16 = badge_text.width() as u16;
        let badge_style = Style::default().fg(theme.accent_plan).bg(theme.bg_base);

        let separator = "  |  ";
        let sep_w: u16 = 5; // separator is fixed-width ASCII; matches modal_window.rs:565
        let sep_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);

        let mut base_w: u16 = 0;
        if action_w > 0 {
            base_w = base_w.saturating_add(action_w).saturating_add(sep_w);
        }
        if revise_w > 0 {
            base_w = base_w.saturating_add(revise_w).saturating_add(sep_w);
        }
        base_w = base_w.saturating_add(comment_w).saturating_add(badge_w);
        if let Some((_, w)) = &quit_spans {
            base_w = base_w.saturating_add(sep_w).saturating_add(*w);
        }
        let with_copy_w = base_w.saturating_add(sep_w).saturating_add(copy_w);
        let show_copy = with_copy_w <= inner.width;
        let total_w = if show_copy { with_copy_w } else { base_w };

        if total_w <= inner.width {
            let mut x = inner.x + (inner.width - total_w) / 2;

            // Action button (approve / send) — left-most.
            if let Some(spans) = &action_spans {
                let approve_x = x;
                for span in spans {
                    let w = span.width() as u16;
                    buf.set_span(x, bottom_y, span, w);
                    x += w;
                }
                viewer.plan_mut().approve_button_area =
                    Some(Rect::new(approve_x, bottom_y, action_w, 1));

                buf.set_string(x, bottom_y, separator, sep_style);
                x += sep_w;
            } else {
                viewer.plan_mut().approve_button_area = None;
            }

            // Revise button — approval mode with comments.
            if let Some(spans) = &revise_spans {
                let revise_x = x;
                for span in spans {
                    let w = span.width() as u16;
                    buf.set_span(x, bottom_y, span, w);
                    x += w;
                }
                viewer.plan_mut().send_button_area =
                    Some(Rect::new(revise_x, bottom_y, revise_w, 1));

                buf.set_string(x, bottom_y, separator, sep_style);
                x += sep_w;
            } else {
                viewer.plan_mut().send_button_area = None;
            }

            // Comment button — always present in both modes.
            let comment_x = x;
            for span in &comment_spans {
                let w = span.width() as u16;
                buf.set_span(x, bottom_y, span, w);
                x += w;
            }
            viewer.plan_mut().comment_button_area =
                Some(Rect::new(comment_x, bottom_y, comment_w, 1));

            if badge_w > 0 {
                buf.set_string(x, bottom_y, &badge_text, badge_style);
                x += badge_w;
            }

            if show_copy {
                buf.set_string(x, bottom_y, separator, sep_style);
                x += sep_w;
                let copy_x = x;
                for span in &copy_spans {
                    let w = span.width() as u16;
                    buf.set_span(x, bottom_y, span, w);
                    x += w;
                }
                viewer.plan_mut().copy_button_area = Some(Rect::new(copy_x, bottom_y, copy_w, 1));
            } else {
                viewer.plan_mut().copy_button_area = None;
            }

            // Quit button — approval mode only.
            if let Some((spans, w)) = quit_spans {
                buf.set_string(x, bottom_y, separator, sep_style);
                x += sep_w;
                let quit_x = x;
                for span in &spans {
                    let sw = span.width() as u16;
                    buf.set_span(x, bottom_y, span, sw);
                    x += sw;
                }
                viewer.plan_mut().abandon_button_area = Some(Rect::new(quit_x, bottom_y, w, 1));
            } else {
                viewer.plan_mut().abandon_button_area = None;
            }
        } else {
            // Footer too narrow — disable hit-tests so stale rects
            // from a previous render don't fire.
            let plan = viewer.plan_mut();
            plan.approve_button_area = None;
            plan.comment_button_area = None;
            plan.copy_button_area = None;
            plan.abandon_button_area = None;
        }
    }
}

pub use crate::render::color::dim_area;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_markdown_content_uses_in_memory_content() {
        let viewer = LineViewerState::open_markdown_content(
            "plan.md",
            "# Plan\n\n- Do the thing".to_owned(),
            None,
        )
        .expect("markdown content should open");

        assert_eq!(viewer.path, PathBuf::from("plan.md"));
        assert_eq!(
            viewer.markdown_content.as_deref(),
            Some("# Plan\n\n- Do the thing")
        );
    }

    #[test]
    fn markdown_content_for_test_returns_in_memory_content() {
        let viewer =
            LineViewerState::open_markdown_content("plan.md", "# Latest Plan".to_owned(), None)
                .expect("markdown content should open");

        assert_eq!(viewer.markdown_content_for_test(), Some("# Latest Plan"));
    }

    #[test]
    fn open_markdown_content_rejects_blank_content() {
        assert!(
            LineViewerState::open_markdown_content("plan.md", "  \n".to_owned(), None).is_none()
        );
    }

    #[test]
    fn plan_preview_exposes_full_raw_markdown_for_copy() {
        let body = "# Plan\n\n- Do the thing\n- Then ship";
        let mut viewer = LineViewerState::open_markdown_content("plan.md", body.to_owned(), None)
            .expect("markdown content should open");
        viewer.kind = LineViewerKind::PlanPreview;
        viewer.prepare_layout(80, 20);

        assert_eq!(
            viewer.markdown_content_for_feedback().as_deref(),
            Some(body)
        );
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn source_line(item: &PlanViewerItem) -> &SourceLine {
        match item {
            PlanViewerItem::Source(source) => source,
            _ => panic!("expected source line"),
        }
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        let area = *buf.area();
        let mut row = String::new();
        for x in area.left()..area.right() {
            row.push_str(buf[(x, y)].symbol());
        }
        row
    }

    #[test]
    fn build_markdown_lines_preserves_blank_source_lines() {
        let built = build_markdown_lines("# Plan\n\n- First\n\n- Second", Some(80));
        let numbered_rows: Vec<(usize, Vec<String>)> = built
            .source_lines
            .iter()
            .map(|line| {
                (
                    line.line_number,
                    line.rendered_lines.iter().map(line_text).collect(),
                )
            })
            .collect();

        assert_eq!(
            numbered_rows,
            vec![
                (1, vec!["Plan".to_owned()]),
                (2, vec![String::new()]),
                (3, vec!["• First".to_owned()]),
                (4, vec![String::new()]),
                (5, vec!["• Second".to_owned()]),
            ]
        );
    }

    #[test]
    fn markdown_source_blank_line_renders_as_numbered_empty_row() {
        let built = build_markdown_lines("# Plan\n\n- First", Some(80));
        let blank = &built.source_lines[1];
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));

        blank.render(Rect::new(0, 0, 20, 1), &mut buf, false, true);

        assert_eq!(blank.line_number, 2);
        assert_eq!(row_text(&buf, 0), "2                   ");
    }

    #[test]
    fn mermaid_affordance_respects_render_setting() {
        use crate::appearance::{RenderMermaid, cache};

        const MD: &str = "# Plan\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\nDone.\n";

        cache::set_render_mermaid(RenderMermaid::On);
        let built = build_markdown_lines(MD, Some(80));
        assert_eq!(built.mermaid_after.len(), 1);
        assert!(built.mermaid_after[0].1.contains("A --> B"));
        assert!(built.mermaid_after[0].0 < built.source_lines.len());

        let mut viewer =
            LineViewerState::open_markdown_content("plan.md", MD.to_owned(), None).unwrap();
        viewer.prepare_layout(100, 40);
        assert_eq!(
            viewer
                .lines
                .iter()
                .filter(|i| matches!(i, PlanViewerItem::MermaidAffordance(_)))
                .count(),
            1
        );
        let placements = viewer.diagram_affordance_placements(Rect::new(0, 0, 100, 40));
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].screen_rect.height, 1);
        assert!(placements[0].screen_rect.width > 0);

        cache::set_render_mermaid(RenderMermaid::Off);
        assert!(build_markdown_lines(MD, Some(80)).mermaid_after.is_empty());
        cache::set_render_mermaid(RenderMermaid::Auto);
    }

    #[test]
    fn markdown_viewer_selection_uses_source_line_numbers_with_blank_rows() {
        let mut viewer = LineViewerState::open_markdown_content(
            "plan.md",
            "# Plan\n\n- First\n\n- Second".to_owned(),
            None,
        )
        .expect("markdown content should open");
        viewer.prepare_layout(80, 20);

        let ids_by_line: Vec<(usize, u64)> = viewer
            .lines
            .iter()
            .map(|item| {
                let source = source_line(item);
                (source.line_number, source.item_id)
            })
            .collect();
        assert_eq!(
            ids_by_line
                .iter()
                .map(|(line, _)| *line)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );

        let line_4_id = ids_by_line
            .iter()
            .find_map(|(line, id)| (*line == 4).then_some(*id))
            .expect("blank source line should have its own row");
        viewer.list_state.select_by_id(line_4_id);
        viewer.prepare_layout(80, 20);

        assert_eq!(viewer.selected_line_range(), Some(4..5));
        assert_eq!(viewer.line_range_suffix(), Some(":4".to_owned()));
        assert_eq!(source_line(&viewer.lines[3]).plain_text, "");
    }

    #[test]
    fn markdown_viewer_preserves_soft_break_collapsed_lines() {
        // Repro: consecutive non-blank lines (a poem) form one
        // CommonMark paragraph. Source-faithful rendering must keep each line
        // on its own numbered row instead of collapsing to one paragraph.
        let mut viewer = LineViewerState::open_markdown_content(
            "plan.md",
            "Line one,\nLine two,\nLine three.".to_owned(),
            None,
        )
        .expect("markdown content should open");
        viewer.prepare_layout(80, 20);

        let rows: Vec<(usize, String)> = viewer
            .lines
            .iter()
            .map(|item| {
                let s = source_line(item);
                (s.line_number, line_text(&s.rendered_lines[0]))
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "Line one,".to_owned()),
                (2, "Line two,".to_owned()),
                (3, "Line three.".to_owned()),
            ]
        );
    }

    #[test]
    fn markdown_viewer_comment_range_maps_full_soft_break_paragraph() {
        // Commenting round-trip: selecting all rows of a soft-break paragraph
        // must map back to the full file line range so the agent inspects the
        // correct lines. Pre-fix this collapsed to a single line number.
        let mut viewer = LineViewerState::open_markdown_content(
            "plan.md",
            "Line one,\nLine two,\nLine three.".to_owned(),
            None,
        )
        .expect("markdown content should open");
        viewer.prepare_layout(80, 20);
        viewer.set_initial_selection(1..4);
        viewer.prepare_layout(80, 20);

        assert_eq!(viewer.selected_line_range(), Some(1..4));
        assert_eq!(viewer.line_range_suffix(), Some(":1-3".to_owned()));
    }
}
