//! Todo pane — renders `TodoItem`s from `echo-agent-tools` in a `ListPane`.
//!
//! Wraps the canonical `TodoItem` type with a `ListItem` implementation
//! that provides status-icon prefixes and styled content.

use echo_agent_runtime::tools::{TodoItem, TodoStatus};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::list_pane::ListItem;

// ---------------------------------------------------------------------------
// TodoPaneStyle — per-status colors
// ---------------------------------------------------------------------------

/// Visual style for each todo status.
#[derive(Debug, Clone, Copy)]
pub struct TodoStatusStyle {
    /// Color for the status icon.
    pub icon_fg: Color,
    /// Style for the content text.
    pub text_style: Style,
}

/// Full style configuration for the todo pane.
#[derive(Debug, Clone, Copy)]
pub struct TodoPaneStyle {
    pub pending: TodoStatusStyle,
    pub in_progress: TodoStatusStyle,
    pub completed: TodoStatusStyle,
    pub cancelled: TodoStatusStyle,
}

impl Default for TodoPaneStyle {
    fn default() -> Self {
        // Sourced from theme to ensure colors are quantized for terminal compat.
        let theme = crate::theme::Theme::current();

        Self {
            pending: TodoStatusStyle {
                icon_fg: theme.text_primary,
                text_style: Style::default().fg(theme.text_primary),
            },
            in_progress: TodoStatusStyle {
                icon_fg: theme.warning,
                text_style: Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            },
            completed: TodoStatusStyle {
                icon_fg: theme.accent_success,
                text_style: Style::default().fg(theme.gray_bright),
            },
            cancelled: TodoStatusStyle {
                icon_fg: theme.accent_error,
                text_style: Style::default()
                    .fg(theme.gray_bright)
                    .add_modifier(Modifier::CROSSED_OUT),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// TodoListEntry — ListItem wrapper around TodoItem
// ---------------------------------------------------------------------------

/// A `TodoItem` wrapped for display in a `ListPane`.
///
/// Caches the styled `Line` for `content()` and generates a status-icon
/// `prefix()` per frame.
#[derive(Debug, Clone)]
pub struct TodoListEntry {
    /// Unique ID (index in the todo list, or a stable ID from the model).
    pub id: u64,
    /// The canonical todo item.
    pub item: TodoItem,
    /// Cached styled content line.
    styled: Line<'static>,
    /// The style to use for this entry's status.
    status_style: TodoStatusStyle,
}

impl TodoListEntry {
    /// Create a new entry from a `TodoItem`.
    pub fn new(id: u64, item: TodoItem, style: &TodoPaneStyle) -> Self {
        let status_style = match item.status {
            TodoStatus::Pending => style.pending,
            TodoStatus::InProgress => style.in_progress,
            TodoStatus::Completed => style.completed,
            TodoStatus::Cancelled => style.cancelled,
        };
        let styled = Line::from(Span::styled(item.content.clone(), status_style.text_style));
        Self {
            id,
            item,
            styled,
            status_style,
        }
    }

    /// Status icon for the current status.
    fn icon(&self) -> &'static str {
        match self.item.status {
            TodoStatus::Pending => "□",
            TodoStatus::InProgress => "▶",
            TodoStatus::Completed => crate::glyphs::check_mark(),
            TodoStatus::Cancelled => crate::glyphs::ballot_x(),
        }
    }
}

impl ListItem for TodoListEntry {
    fn content(&self) -> &Line<'_> {
        &self.styled
    }

    fn prefix(&self) -> Option<Line<'_>> {
        let icon = self.icon();
        Some(Line::from(vec![
            Span::styled(icon, Style::default().fg(self.status_style.icon_fg)),
            Span::raw(" "),
        ]))
    }

    fn stable_id(&self) -> u64 {
        self.id
    }

    fn search_text(&self) -> &str {
        &self.item.content
    }
}

// ---------------------------------------------------------------------------
// TodoPane — self-contained pane owning items, state, and rendering
// ---------------------------------------------------------------------------

use crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::StatefulWidget;

use crate::appearance::LayoutConfig;
use crate::theme::ThemeKind;

use super::list_pane::{ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode};
use super::overlay::OverlayState;

// ---------------------------------------------------------------------------
// TodoCounts — aggregate status counts
// ---------------------------------------------------------------------------

/// Counts of todo items by status.
///
/// Feeds the empty-pane placeholder message.
/// Counts ALL items regardless of `show_done` filter.
#[derive(Debug, Clone, Copy, Default)]
pub struct TodoCounts {
    pub in_progress: usize,
    pub pending: usize,
    pub completed: usize,
    pub cancelled: usize,
}

impl TodoCounts {
    /// Total number of items across all statuses.
    pub fn total(&self) -> usize {
        self.in_progress + self.pending + self.completed + self.cancelled
    }
}

fn empty_placeholder_message(todos_empty: bool, counts: TodoCounts) -> String {
    if todos_empty {
        return "No todo items.".into();
    }
    match (counts.completed, counts.cancelled) {
        (_, 0) => "All done.".into(),
        (0, c) => format!("{c} cancelled."),
        (d, c) => format!("{d} done. {c} cancelled."),
    }
}

/// Absolute maximum height (in lines) the todo pane will request.
const MAX_TODO_HEIGHT: u16 = 10;

/// Maximum fraction of total view height the todo pane may occupy.
const MAX_TODO_FRACTION: f32 = 0.15;

/// Self-contained todo pane component.
///
/// Owns the raw `TodoItem` data, the filtered `TodoListEntry` cache,
/// `ListPaneState`, and style config. `AgentView` holds a single
/// `TodoPane` and delegates input/render to it.
pub struct TodoPane {
    /// Raw todo items from ACP Plan updates.
    todos: Vec<TodoItem>,
    /// Filtered + styled entries for `ListPane` rendering.
    /// Rebuilt from `todos` at the start of each `render()` call.
    entries: Vec<TodoListEntry>,
    /// List pane state (scroll, selection, search, layout cache).
    pub list_state: ListPaneState,
    /// Per-status visual style for icons and text.
    style: TodoPaneStyle,
    /// Visual style for the list pane framework (selection bg, etc.).
    list_style: ListPaneStyle,
    /// Whether to show completed/cancelled items.
    show_done: bool,
    /// Shared visibility/focus state.
    pub overlay: OverlayState,
    /// Last theme kind seen — used to detect theme switches and restyle.
    last_theme: ThemeKind,
}

impl Default for TodoPane {
    fn default() -> Self {
        Self::new()
    }
}

impl TodoPane {
    /// Create a new empty todo pane.
    pub fn new() -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: false,
            visual_select_enabled: false,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let mut list_state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
        list_state.set_clipboard_provider(Box::new(crate::clipboard::SystemClipboard));
        Self {
            todos: Vec::new(),
            entries: Vec::new(),
            list_state,
            style: TodoPaneStyle::default(),
            list_style: ListPaneStyle::default(),
            show_done: true,
            overlay: OverlayState::hidden(),
            last_theme: crate::theme::Theme::current_kind(),
        }
    }

    // -- Data management -----------------------------------------------------

    /// Read-only access to the current todo items.
    pub fn todos(&self) -> &[TodoItem] {
        &self.todos
    }

    /// Replace all todo items (called from ACP Plan handler).
    ///
    /// Does NOT auto-show the todo pane. Users toggle it with Ctrl-T.
    pub fn update_todos(&mut self, items: Vec<TodoItem>) {
        self.todos = items;
    }

    /// Compute status counts from a list of items.
    fn compute_counts(items: &[TodoItem]) -> TodoCounts {
        let mut c = TodoCounts::default();
        for item in items {
            match item.status {
                TodoStatus::InProgress => c.in_progress += 1,
                TodoStatus::Pending => c.pending += 1,
                TodoStatus::Completed => c.completed += 1,
                TodoStatus::Cancelled => c.cancelled += 1,
            }
        }
        c
    }

    /// Current status counts (across ALL items, ignoring `show_done`).
    pub fn counts(&self) -> TodoCounts {
        Self::compute_counts(&self.todos)
    }

    /// Whether the pane should be visible in the layout.
    ///
    /// Visible when overlay is shown. When empty, shows a placeholder
    /// message instead of the list pane.
    pub fn is_visible(&self) -> bool {
        self.overlay.visible
    }

    /// Called after overlay state changes that hide the pane.
    ///
    /// Closes the input bar if it's actively open (mid-typing), but
    /// preserves any accepted search/filter so it persists across
    /// show/hide cycles.
    pub fn on_state_change(&mut self) {
        if !self.overlay.visible {
            self.list_state.close_input_bar();
        }
    }

    /// Whether completed/cancelled items are shown.
    pub fn show_done(&self) -> bool {
        self.show_done
    }

    /// Toggle visibility of completed/cancelled items.
    pub fn toggle_show_done(&mut self) {
        self.show_done = !self.show_done;
    }

    /// Desired height in lines for layout computation.
    ///
    /// Returns 0 when hidden (overlay not visible or no items).
    /// When visible but empty, returns 2 (for placeholder message).
    /// Otherwise: `min(10, 15% of view_height)` but at least 1.
    pub fn desired_height(&self, view_height: u16) -> u16 {
        if !self.overlay.visible {
            return 0;
        }
        let count = self.visible_count();
        if count == 0 {
            return 1;
        }
        let fraction_cap = (view_height as f32 * MAX_TODO_FRACTION).floor() as u16;
        let max = MAX_TODO_HEIGHT.min(fraction_cap).max(1);
        (count as u16).min(max).max(1)
    }

    /// Number of items that pass the `show_done` filter.
    fn visible_count(&self) -> usize {
        if self.show_done {
            self.todos.len()
        } else {
            self.todos
                .iter()
                .filter(|t| !matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
                .count()
        }
    }

    /// Rebuild `entries` from `todos`, filtered by `show_done`.
    ///
    /// Items preserve their original order from the agent (no status-based
    /// reordering). IDs are based on the original index in `todos` so that
    /// `ListPaneState` can maintain selection across rebuilds.
    fn rebuild_entries(&mut self) {
        self.entries.clear();
        for (idx, item) in self.todos.iter().enumerate() {
            if !self.show_done
                && matches!(item.status, TodoStatus::Completed | TodoStatus::Cancelled)
            {
                continue;
            }
            self.entries
                .push(TodoListEntry::new(idx as u64, item.clone(), &self.style));
        }
    }

    // -- Input handling ------------------------------------------------------

    /// Handle a key event when the todo pane is focused.
    ///
    /// Returns `true` if the event was consumed.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        // 'h' toggles show_done (only when not typing in search/filter bar).
        if key.code == KeyCode::Char('h') && self.list_state.input_mode().is_none() {
            self.toggle_show_done();
            self.rebuild_entries();
            return true;
        }
        // Don't route to ListPaneState when empty.
        if self.entries.is_empty() {
            return false;
        }
        self.list_state.handle_key_event(key, &self.entries)
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.list_state.handle_paste(text, &self.entries)
    }

    /// Handle a mouse scroll event over the todo pane area.
    ///
    /// Caps scroll speed for small viewports — the app-level scroll
    /// accumulator can produce large deltas (3-5 lines) which would
    /// jump past most items in a 4-row pane.
    pub fn handle_scroll(&mut self, lines: i32, col: u16, row: u16) {
        let max = match self.list_state.viewport_height() {
            0..=5 => 1,
            6..=10 => 2,
            _ => lines.unsigned_abs() as i32,
        };
        let capped = lines.signum() * lines.abs().min(max);
        self.list_state
            .handle_scroll_event(capped, col, row, &self.entries);
    }

    /// Handle a mouse click/drag event over the todo pane area.
    ///
    /// Returns `true` if the event was consumed.
    pub fn handle_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16, area: Rect) -> bool {
        self.list_state
            .handle_mouse_event(kind, col, row, area, &self.entries)
    }

    // -- Rendering -----------------------------------------------------------

    /// Compute the inner content area with horizontal padding matching
    /// the scrollback's `HorizontalLayout` (accent + block_pad_left on
    /// the left, block_pad_right on the right).
    fn content_area(area: Rect, layout_cfg: &LayoutConfig) -> Rect {
        use crate::scrollback::layout::HorizontalLayout;
        let pad_left = HorizontalLayout::ACCENT + layout_cfg.block_pad_left;
        let pad_right = layout_cfg.block_pad_right;
        Rect {
            x: area.x + pad_left,
            y: area.y,
            width: area.width.saturating_sub(pad_left + pad_right),
            height: area.height,
        }
    }

    /// Render the todo pane into the given area.
    ///
    /// Rebuilds entries from `todos` each call (cheap — typically <20 items),
    /// runs layout, and renders the `ListPane` widget in a padded inner area
    /// matching the scrollback's horizontal layout.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        focused: bool,
        layout_cfg: &LayoutConfig,
    ) {
        // Detect theme switch and refresh styles before rebuilding entries.
        let current_theme = crate::theme::Theme::current_kind();
        if current_theme != self.last_theme {
            self.last_theme = current_theme;
            self.style = TodoPaneStyle::default();
            self.list_style = ListPaneStyle::default();
        }

        self.rebuild_entries();
        let inner = Self::content_area(area, layout_cfg);
        if self.entries.is_empty() {
            // Empty state: placeholder message in muted style.
            if inner.height > 0 && inner.width > 0 {
                let msg = empty_placeholder_message(self.todos.is_empty(), self.counts());
                let theme = crate::theme::Theme::current();
                let span = ratatui::text::Span::styled(
                    msg,
                    ratatui::style::Style::default().fg(theme.gray_bright),
                );
                buf.set_span(inner.x, inner.y, &span, inner.width);
            }
            return;
        }
        self.list_state
            .prepare_layout(&self.entries, inner.width, inner.height);
        ListPane::new(&self.entries)
            .focused(focused)
            .style(self.list_style)
            .render(inner, buf, &mut self.list_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(completed: usize, cancelled: usize) -> TodoCounts {
        TodoCounts {
            completed,
            cancelled,
            ..TodoCounts::default()
        }
    }

    #[test]
    fn empty_todos_message() {
        assert_eq!(
            empty_placeholder_message(true, TodoCounts::default()),
            "No todo items."
        );
    }

    #[test]
    fn all_completed_is_all_done() {
        assert_eq!(empty_placeholder_message(false, counts(3, 0)), "All done.");
    }

    #[test]
    fn mixed_done_and_cancelled_summarizes_counts() {
        assert_eq!(
            empty_placeholder_message(false, counts(5, 1)),
            "5 done. 1 cancelled."
        );
    }

    #[test]
    fn only_cancelled() {
        assert_eq!(
            empty_placeholder_message(false, counts(0, 2)),
            "2 cancelled."
        );
    }
}
